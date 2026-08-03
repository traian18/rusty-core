//! Subprocess `ExecutionBackend` wrapping the Claude Code CLI.
//!
//! Structurally different from the HTTP-API integrations
//! (anthropic/openai/gemini/openai-compatible): there is no `ModelClient` /
//! `GenericModelBackend` here at all. The CLI already does its own tool use,
//! context management, and multi-turn looping internally, so this backend's
//! job is just to drive it as a child process and translate *its* JSON-lines
//! output directly into [`ExecutionEvent`]s.

use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use rust_decimal::Decimal;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
    ExecutionResult,
};
use harness_protocol::ids::BackendId;
use harness_protocol::usage::{Cost, CostSource};
use harness_runtime::traits::ExecutionBackend;
use harness_runtime::IntegrationFactory;

use crate::config::ClaudeCodeConfig;
use crate::wire::{extract_assistant_text, extract_latest_user_text, extract_result, extract_session_id};

/// Drives the Claude Code CLI as a subprocess `ExecutionBackend`.
///
/// One `claude -p` process is spawned per [`execute`](Self::execute) call
/// (matching how the CLI itself is designed to be invoked — each run is a
/// complete, short-lived process); conversation continuity across calls
/// comes from `--resume <session_id>`, not a long-lived pipe, since the CLI
/// persists its own session state on disk. Only the newest user turn is
/// sent on each call — see [`extract_latest_user_text`].
pub struct ClaudeCodeBackend {
    config: ClaudeCodeConfig,
    session_id: Mutex<Option<String>>,
}

impl ClaudeCodeBackend {
    pub fn new(config: ClaudeCodeConfig) -> Self {
        Self {
            config,
            session_id: Mutex::new(None),
        }
    }
}

#[async_trait]
impl ExecutionBackend for ClaudeCodeBackend {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: BackendId::new(),
            name: "claude-code".to_string(),
            description: "Claude Code CLI driven as a subprocess".to_string(),
            capabilities: self.capabilities(),
        }
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            streaming: true,
            reasoning_stream: true,
            // Tool calls happen entirely inside the CLI subprocess and are
            // never relayed back as ExecutionEvent::ToolCallRequested — the
            // harness never sees or executes them.
            tool_calls: false,
            parallel_tool_calls: false,
            // host_managed_tools = false: the *host* (this harness) does
            // NOT manage tool execution for this backend — the CLI does,
            // entirely internally. This is the opposite of the HTTP-API
            // integrations, which set it true.
            host_managed_tools: false,
            ..Default::default()
        }
    }

    async fn execute(
        &self,
        request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        let prompt = extract_latest_user_text(&request.messages).ok_or_else(|| {
            ExecutionError::InvalidRequest {
                message: "no user message to send to the claude CLI".to_string(),
            }
        })?;

        let resume_id = self.session_id.lock().expect("session_id mutex poisoned").clone();

        let mut command = tokio::process::Command::new(&self.config.binary_path);
        command
            .arg("-p")
            .arg(&prompt)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--permission-mode")
            .arg(&self.config.permission_mode)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if !request.system_prompt.is_empty() {
            command.arg("--append-system-prompt").arg(&request.system_prompt);
        }
        if let Some(dir) = &self.config.working_dir {
            command.current_dir(dir);
        }
        if let Some(id) = &resume_id {
            command.arg("--resume").arg(id);
        }
        command.args(&self.config.extra_args);

        let mut child = command.spawn().map_err(|error| ExecutionError::BackendError {
            message: format!("failed to spawn claude CLI: {error}"),
            code: "spawn_failed".to_string(),
        })?;

        // Surface the CLI's own diagnostics as tracing output rather than
        // silently discarding them or merging them into stdout (which would
        // corrupt the JSON-lines parsing).
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "claude_code_cli_stderr", "{line}");
                }
            });
        }

        let stdout = child.stdout.take().expect("stdout was piped");
        let mut lines = BufReader::new(stdout).lines();

        let mut sent_text = String::new();
        let mut new_session_id: Option<String> = None;
        let mut final_result: Option<ExecutionResult> = None;

        loop {
            let line = tokio::select! {
                _ = cancel.cancelled() => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    return Err(ExecutionError::Cancelled);
                }
                line = lines.next_line() => line,
            };
            let line = line.map_err(|error| ExecutionError::BackendError {
                message: format!("failed to read claude CLI output: {error}"),
                code: "io_error".to_string(),
            })?;
            let Some(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }

            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue; // tolerate stray non-JSON noise on stdout
            };

            if let Some(id) = extract_session_id(&value) {
                new_session_id = Some(id);
                continue;
            }

            if let Some(full_text) = extract_assistant_text(&value) {
                // The CLI emits the *full* accumulated text per line, not a
                // delta — diff against what's already been forwarded.
                if let Some(delta) = full_text.strip_prefix(sent_text.as_str()) {
                    if !delta.is_empty() {
                        let _ = sink.send(ExecutionEvent::TextDelta {
                            request_id: request.request_id,
                            delta: delta.to_string(),
                        });
                    }
                } else if full_text != sent_text {
                    // Text was replaced rather than appended (shouldn't
                    // normally happen) — forward it rather than lose it.
                    let _ = sink.send(ExecutionEvent::TextDelta {
                        request_id: request.request_id,
                        delta: full_text.clone(),
                    });
                }
                sent_text = full_text;
                continue;
            }

            if let Some(result_line) = extract_result(&value) {
                let cost = Cost {
                    amount_usd: result_line.cost_usd.and_then(Decimal::from_f64_retain),
                    source: Some(CostSource::ProviderReported),
                };
                final_result = Some(ExecutionResult {
                    request_id: request.request_id,
                    usage: result_line.usage,
                    cost,
                    finish_reason: result_line.finish_reason,
                });
            }
        }

        let status = child.wait().await.map_err(|error| ExecutionError::BackendError {
            message: format!("claude CLI process error: {error}"),
            code: "wait_failed".to_string(),
        })?;

        if let Some(id) = new_session_id {
            *self.session_id.lock().expect("session_id mutex poisoned") = Some(id);
        }

        match final_result {
            Some(result) => {
                let _ = sink.send(ExecutionEvent::Completed {
                    request_id: request.request_id,
                    result: result.clone(),
                });
                Ok(result)
            }
            None => Err(ExecutionError::BackendError {
                message: format!("claude CLI exited ({status}) without a result line"),
                code: "no_result".to_string(),
            }),
        }
    }
}

/// Registry factory for the `claude-code` integration family.
pub struct ClaudeCodeFactory;

#[async_trait]
impl IntegrationFactory for ClaudeCodeFactory {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn descriptor(&self) -> BackendDescriptor {
        ClaudeCodeBackend::new(ClaudeCodeConfig::default()).descriptor()
    }

    async fn create(
        &self,
        config: serde_json::Value,
    ) -> Result<Arc<dyn ExecutionBackend>, Box<dyn std::error::Error + Send + Sync>> {
        let binary_path = config
            .get("binary_path")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| ClaudeCodeConfig::default().binary_path);
        let permission_mode = config
            .get("permission_mode")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| ClaudeCodeConfig::default().permission_mode);
        let working_dir = config
            .get("working_dir")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from);
        let extra_args = config
            .get("extra_args")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();

        Ok(Arc::new(ClaudeCodeBackend::new(ClaudeCodeConfig {
            binary_path,
            extra_args,
            permission_mode,
            working_dir,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_reflect_cli_owned_tools() {
        let backend = ClaudeCodeBackend::new(ClaudeCodeConfig::default());
        let capabilities = backend.capabilities();
        assert!(!capabilities.host_managed_tools);
        assert!(!capabilities.tool_calls);
        assert!(capabilities.streaming);
    }

    #[tokio::test]
    async fn factory_uses_defaults_for_a_minimal_config() {
        let backend = ClaudeCodeFactory
            .create(serde_json::json!({}))
            .await
            .expect("valid minimal config");
        assert!(!backend.capabilities().host_managed_tools);
    }

    // ---------------------------------------------------------------------
    // Stub-binary tests: a fake "claude" script emits canned JSON lines
    // matching the real schema (recorded from an actual `claude -p ...
    // --output-format stream-json --verbose` invocation), so these exercise
    // the real spawn/parse/kill code paths without needing the real CLI
    // installed in CI.
    // ---------------------------------------------------------------------

    use harness_protocol::ids::{MessageId, RequestId, RunId, Timestamp};
    use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};

    fn user_message(text: &str) -> AgentMessage {
        AgentMessage {
            id: MessageId::new(),
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: text.to_string() }],
            created_at: Timestamp::now(),
        }
    }

    fn request_with(messages: Vec<AgentMessage>) -> ExecutionRequest {
        ExecutionRequest {
            request_id: RequestId::new(),
            run_id: RunId::new(),
            system_prompt: String::new(),
            messages,
            tools: vec![],
            extended_thinking: false,
        }
    }

    /// Writes an executable shell script at `path` that ignores its
    /// arguments and prints `body` to stdout.
    #[cfg(unix)]
    fn write_stub_script(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let script = format!("#!/bin/sh\ncat <<'STUB_EOF'\n{body}\nSTUB_EOF\n");
        std::fs::write(path, script).expect("write stub script");
        let mut perms = std::fs::metadata(path).expect("stat stub script").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod stub script");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stub_cli_produces_text_deltas_and_completes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("fake_claude.sh");
        write_stub_script(
            &script_path,
            r#"{"type":"system","subtype":"init","session_id":"11111111-1111-1111-1111-111111111111"}
{"type":"assistant","message":{"content":[{"type":"text","text":"Hello, "}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"Hello, world!"}]}}
{"type":"result","subtype":"success","total_cost_usd":0.01,"usage":{"input_tokens":10,"output_tokens":5},"result":"Hello, world!"}"#,
        );

        let backend = ClaudeCodeBackend::new(ClaudeCodeConfig {
            binary_path: script_path,
            ..ClaudeCodeConfig::default()
        });

        let (tx, mut rx) = broadcast::channel(16);
        let result = backend
            .execute(request_with(vec![user_message("hi")]), tx, CancellationToken::new())
            .await
            .expect("execute should succeed");

        assert_eq!(result.finish_reason, "success");
        assert_eq!(result.usage.input_tokens.value(), Some(10));
        assert_eq!(result.usage.output_tokens.value(), Some(5));

        let mut deltas = String::new();
        let mut saw_completed = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                ExecutionEvent::TextDelta { delta, .. } => deltas.push_str(&delta),
                ExecutionEvent::Completed { .. } => saw_completed = true,
                _ => {}
            }
        }
        assert_eq!(deltas, "Hello, world!");
        assert!(saw_completed);

        // The session id from the init line should now be threaded into
        // --resume on the next call.
        assert_eq!(
            *backend.session_id.lock().unwrap(),
            Some("11111111-1111-1111-1111-111111111111".to_string())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stub_cli_error_without_a_result_line_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("fake_claude_no_result.sh");
        write_stub_script(
            &script_path,
            r#"{"type":"system","subtype":"init","session_id":"22222222-2222-2222-2222-222222222222"}
{"type":"assistant","message":{"content":[{"type":"text","text":"partial"}]}}"#,
        );

        let backend = ClaudeCodeBackend::new(ClaudeCodeConfig {
            binary_path: script_path,
            ..ClaudeCodeConfig::default()
        });

        let (tx, _rx) = broadcast::channel(16);
        let result = backend
            .execute(request_with(vec![user_message("hi")]), tx, CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_without_a_user_message_is_rejected_before_spawning() {
        let backend = ClaudeCodeBackend::new(ClaudeCodeConfig {
            binary_path: "/nonexistent/binary/should/never/be/invoked".into(),
            ..ClaudeCodeConfig::default()
        });
        let (tx, _rx) = broadcast::channel(16);
        let result = backend
            .execute(request_with(vec![]), tx, CancellationToken::new())
            .await;
        assert!(matches!(result, Err(ExecutionError::InvalidRequest { .. })));
    }
}
