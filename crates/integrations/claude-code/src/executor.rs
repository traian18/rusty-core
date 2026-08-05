use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tracing::{debug, warn};
use serde_json::{json, Value};

use harness_protocol::backend::ExecutionRequest;
use harness_protocol::backend::{ExecutionEvent, ExecutionError};

use crate::config::ClaudeCodeConfig;

/// Spawns a Claude Code CLI subprocess and manages its lifecycle.
pub struct ClaudeCodeExecutor {
    config: ClaudeCodeConfig,
}

impl ClaudeCodeExecutor {
    pub fn new(config: ClaudeCodeConfig) -> Self {
        Self { config }
    }

    /// Spawn the Claude Code CLI subprocess with the given execution request.
    pub async fn spawn(&self, request: &ExecutionRequest) -> Result<Child, ExecutionError> {
        let mut cmd = tokio::process::Command::new(&self.config.binary_path);
        cmd.arg("--print")
            .arg("--output-format")
            .arg("stream-json")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Add extra CLI arguments
        for arg in &self.config.extra_args {
            cmd.arg(arg);
        }

        // Add permission mode flag (if not default)
        if self.config.permission_mode != "interactive" {
            cmd.arg("--permission-mode").arg(&self.config.permission_mode);
        }

        let mut child = cmd.spawn().map_err(|e| ExecutionError::BackendError {
            message: format!("Failed to spawn Claude Code CLI: {}", e),
            code: "SPAWN_FAILED".to_string(),
        })?;

        // Write the request to stdin: system prompt + messages
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let prompt = format_prompt(request);
            stdin.write_all(prompt.as_bytes()).await.map_err(|e| ExecutionError::BackendError {
                message: format!("Failed to write prompt to Claude CLI: {}", e),
                code: "WRITE_FAILED".to_string(),
            })?;
            stdin.flush().await.map_err(|e| ExecutionError::BackendError {
                message: format!("Failed to flush stdin to Claude CLI: {}", e),
                code: "FLUSH_FAILED".to_string(),
            })?;
            drop(stdin); // Close stdin to signal EOF to the CLI
        }

        Ok(child)
    }

    /// Read stream-json events from the CLI's stdout.
    pub async fn read_events(
        &self,
        mut child: Child,
        request_id: harness_protocol::ids::RequestId,
    ) -> Result<Vec<ExecutionEvent>, ExecutionError> {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ExecutionError::BackendError {
                message: "Claude Code CLI stdout not captured".to_string(),
                code: "NO_STDOUT".to_string(),
            })?;

        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        let mut events = Vec::new();

        while let Some(line) = lines.next_line().await.map_err(|e| ExecutionError::BackendError {
            message: format!("Failed to read from Claude CLI stdout: {}", e),
            code: "READ_FAILED".to_string(),
        })? {
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<Value>(&line) {
                Ok(json) => {
                    if let Some(event) = parse_stream_json_event(&json, request_id) {
                        events.push(event);
                    }
                }
                Err(e) => {
                    warn!("Failed to parse stream-json line: {} (error: {})", line, e);
                }
            }
        }

        // Wait for the child process to exit
        let status = child.wait().await.map_err(|e| ExecutionError::BackendError {
            message: format!("Failed to wait for Claude Code CLI: {}", e),
            code: "WAIT_FAILED".to_string(),
        })?;

        debug!("Claude Code CLI exited with status: {}", status);

        Ok(events)
    }
}

/// Format an ExecutionRequest as a prompt for the Claude Code CLI.
fn format_prompt(request: &ExecutionRequest) -> String {
    let mut prompt = String::new();

    prompt.push_str(&request.system_prompt);
    prompt.push_str("\n\n");

    for message in &request.messages {
        prompt.push_str("---\n");
        prompt.push_str(&format!("Role: {:?}\n", message.role));
        prompt.push_str("Content: ");

        for block in &message.content {
            match block {
                harness_protocol::messages::ContentBlock::Text { text } => prompt.push_str(text),
                other => {
                    if let Ok(s) = serde_json::to_string(other) {
                        prompt.push_str(&s);
                    }
                }
            }
        }

        prompt.push_str("\n");
    }

    prompt
}

/// Parse a single stream-json line from Claude Code CLI output.
fn parse_stream_json_event(
    json: &Value,
    request_id: harness_protocol::ids::RequestId,
) -> Option<ExecutionEvent> {
    let event_type = json.get("type")?.as_str()?;

    match event_type {
        "text_delta" => {
            let text = json.get("text")?.as_str()?;
            Some(ExecutionEvent::TextDelta {
                request_id,
                delta: text.to_string(),
            })
        }
        "tool_call_requested" => {
            let _call_id = json.get("call_id")?.as_str()?;
            let tool_name = json.get("tool_name")?.as_str()?;
            let arguments = json.get("tool_input").map(|v| v.clone()).unwrap_or(json!({}));

            Some(ExecutionEvent::ToolCallRequested {
                request_id,
                call: harness_protocol::tools::ToolCall {
                    id: harness_protocol::ids::ToolCallId::new(),
                    name: tool_name.to_string(),
                    arguments,
                },
            })
        }
        "completion" => {
            let usage = harness_protocol::usage::ModelUsage::default();
            let result = harness_protocol::backend::ExecutionResult {
                request_id,
                usage,
                cost: harness_protocol::usage::Cost {
                    amount_usd: None,
                    source: None,
                },
                finish_reason: "end_turn".to_string(),
            };
            Some(ExecutionEvent::Completed {
                request_id,
                result,
            })
        }
        _ => {
            debug!("Unknown stream-json event type: {}", event_type);
            None
        }
    }
}
