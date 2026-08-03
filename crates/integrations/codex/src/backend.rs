//! Subprocess `ExecutionBackend` wrapping the Codex CLI.
//!
//! Same shape as `harness-integration-claude-code`: no `ModelClient` /
//! `GenericModelBackend` here — the CLI does its own tool use and context
//! management, so this backend drives it as a child process and translates
//! its JSON-lines output directly into [`ExecutionEvent`]s. See
//! `crates/integrations/codex/PLAN.md` and `wire.rs`'s module docs for how
//! the wire schema differs from Claude Code's.

use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
    ExecutionResult,
};
use harness_protocol::ids::BackendId;
use harness_protocol::messages::{ContentBlock, MessageRole};
use harness_protocol::usage::Cost;
use harness_runtime::traits::ExecutionBackend;
use harness_runtime::IntegrationFactory;

use crate::config::CodexConfig;
use crate::wire::{extract_agent_message_text, extract_error, extract_thread_id, extract_turn_completed};

/// Finds the most recent `User`-role message's concatenated `Text` content —
/// the one new turn to send, since a resumed Codex thread already holds
/// every earlier turn (mirrors `harness-integration-claude-code`'s
/// `extract_latest_user_text`).
fn latest_user_text(messages: &[harness_protocol::messages::AgentMessage]) -> Option<String> {
    let message = messages.iter().rev().find(|m| m.role == MessageRole::User)?;
    let text: String = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    (!text.is_empty()).then_some(text)
}

/// Drives the Codex CLI as a subprocess `ExecutionBackend`. One `codex exec`
/// process is spawned per [`execute`](Self::execute) call; continuity across
/// calls comes from `codex exec resume <thread_id>`, threaded from the
/// `thread.started` line of the prior call.
pub struct CodexBackend {
    config: CodexConfig,
    thread_id: Mutex<Option<String>>,
}

impl CodexBackend {
    pub fn new(config: CodexConfig) -> Self {
        Self {
            config,
            thread_id: Mutex::new(None),
        }
    }
}

#[async_trait]
impl ExecutionBackend for CodexBackend {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: BackendId::new(),
            name: "codex".to_string(),
            description: "Codex CLI driven as a subprocess".to_string(),
            capabilities: self.capabilities(),
        }
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            streaming: true,
            reasoning_stream: true,
            // As with Claude Code: tool calls (shell commands, file edits)
            // happen entirely inside the CLI subprocess's own sandbox and
            // are never relayed back to the harness.
            tool_calls: false,
            parallel_tool_calls: false,
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
        let prompt = latest_user_text(&request.messages).ok_or_else(|| ExecutionError::InvalidRequest {
            message: "no user message to send to the codex CLI".to_string(),
        })?;

        let resume_id = self.thread_id.lock().expect("thread_id mutex poisoned").clone();

        let mut command = tokio::process::Command::new(&self.config.binary_path);
        command.arg("exec");

        match &resume_id {
            Some(id) => {
                // `codex exec resume` doesn't accept --sandbox or -C — it
                // inherits the original session's policy and directory
                // (verified against CLI 0.146.0).
                command.arg("resume").arg("--json").arg("--skip-git-repo-check").arg(id);
            }
            None => {
                command.arg("--json").arg("--skip-git-repo-check");
                if self.config.dangerously_bypass {
                    command.arg("--dangerously-bypass-approvals-and-sandbox");
                } else {
                    command.arg("--sandbox").arg(&self.config.sandbox_mode);
                }
                if let Some(dir) = &self.config.working_dir {
                    command.arg("-C").arg(dir);
                }
            }
        }
        // `-C` alone isn't enough — the native codex binary also needs the
        // *actual* OS-level working directory set, not just the logical
        // flag (confirmed empirically: without this, the process fails
        // with an ENOENT-style error before emitting any JSON at all).
        if let Some(dir) = &self.config.working_dir {
            command.current_dir(dir);
        }
        command.args(&self.config.extra_args);
        command.arg(&prompt);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|error| ExecutionError::BackendError {
            message: format!("failed to spawn codex CLI: {error}"),
            code: "spawn_failed".to_string(),
        })?;

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "codex_cli_stderr", "{line}");
                }
            });
        }

        let stdout = child.stdout.take().expect("stdout was piped");
        let mut lines = BufReader::new(stdout).lines();

        let mut new_thread_id: Option<String> = None;
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
                message: format!("failed to read codex CLI output: {error}"),
                code: "io_error".to_string(),
            })?;
            let Some(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }

            // Tolerate stray non-JSON noise on stdout (observed on a fresh
            // session before auth/session setup finishes) rather than
            // failing the whole run on one unparsable line.
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };

            if let Some(id) = extract_thread_id(&value) {
                new_thread_id = Some(id);
                continue;
            }

            if let Some(message) = extract_error(&value) {
                return Err(ExecutionError::BackendError {
                    message,
                    code: "codex_error".to_string(),
                });
            }

            if let Some(text) = extract_agent_message_text(&value) {
                // Each completed item is whole on arrival (unlike Claude
                // Code's ever-growing accumulation) — forward it directly.
                if !text.is_empty() {
                    let _ = sink.send(ExecutionEvent::TextDelta {
                        request_id: request.request_id,
                        delta: text,
                    });
                }
                continue;
            }

            if let Some(turn) = extract_turn_completed(&value) {
                final_result = Some(ExecutionResult {
                    request_id: request.request_id,
                    usage: turn.usage,
                    // Codex's CLI doesn't report a cost figure at all
                    // (ChatGPT-plan usage isn't metered per-token the way an
                    // API key is) — always unknown for this backend.
                    cost: Cost::default(),
                    finish_reason: "completed".to_string(),
                });
            }
        }

        let status = child.wait().await.map_err(|error| ExecutionError::BackendError {
            message: format!("codex CLI process error: {error}"),
            code: "wait_failed".to_string(),
        })?;

        if let Some(id) = new_thread_id {
            *self.thread_id.lock().expect("thread_id mutex poisoned") = Some(id);
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
                message: format!("codex CLI exited ({status}) without a turn.completed line"),
                code: "no_result".to_string(),
            }),
        }
    }
}

/// Registry factory for the `codex` integration family.
pub struct CodexFactory;

#[async_trait]
impl IntegrationFactory for CodexFactory {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn descriptor(&self) -> BackendDescriptor {
        CodexBackend::new(CodexConfig::default()).descriptor()
    }

    async fn create(
        &self,
        config: serde_json::Value,
    ) -> Result<Arc<dyn ExecutionBackend>, Box<dyn std::error::Error + Send + Sync>> {
        let defaults = CodexConfig::default();
        let binary_path = config
            .get("binary_path")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from)
            .unwrap_or(defaults.binary_path);
        let sandbox_mode = config
            .get("sandbox_mode")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or(defaults.sandbox_mode);
        let dangerously_bypass = config
            .get("dangerously_bypass")
            .and_then(|v| v.as_bool())
            .unwrap_or(defaults.dangerously_bypass);
        let working_dir = config
            .get("working_dir")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from);
        let extra_args = config
            .get("extra_args")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();

        Ok(Arc::new(CodexBackend::new(CodexConfig {
            binary_path,
            extra_args,
            sandbox_mode,
            dangerously_bypass,
            working_dir,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_reflect_cli_owned_tools() {
        let backend = CodexBackend::new(CodexConfig::default());
        let capabilities = backend.capabilities();
        assert!(!capabilities.host_managed_tools);
        assert!(!capabilities.tool_calls);
        assert!(capabilities.streaming);
    }

    #[tokio::test]
    async fn factory_uses_defaults_for_a_minimal_config() {
        let backend = CodexFactory
            .create(serde_json::json!({}))
            .await
            .expect("valid minimal config");
        assert!(!backend.capabilities().host_managed_tools);
    }

    use harness_protocol::ids::{MessageId, RequestId, RunId, Timestamp};
    use harness_protocol::messages::AgentMessage;

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
    async fn stub_cli_forwards_agent_messages_and_completes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("fake_codex.sh");
        write_stub_script(
            &script_path,
            r#"{"type":"thread.started","thread_id":"019fc8bb-b347-7550-87fa-e57e0c0f52df"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"pong"}}
{"type":"turn.completed","usage":{"input_tokens":14976,"cached_input_tokens":11008,"cache_write_input_tokens":0,"output_tokens":5,"reasoning_output_tokens":0}}"#,
        );

        let backend = CodexBackend::new(CodexConfig {
            binary_path: script_path,
            ..CodexConfig::default()
        });

        let (tx, mut rx) = broadcast::channel(16);
        let result = backend
            .execute(request_with(vec![user_message("hi")]), tx, CancellationToken::new())
            .await
            .expect("execute should succeed");

        assert_eq!(result.finish_reason, "completed");
        assert_eq!(result.usage.input_tokens.value(), Some(14976));
        assert_eq!(result.usage.output_tokens.value(), Some(5));
        assert!(result.cost.amount_usd.is_none());

        let mut text = String::new();
        let mut saw_completed = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                ExecutionEvent::TextDelta { delta, .. } => text.push_str(&delta),
                ExecutionEvent::Completed { .. } => saw_completed = true,
                _ => {}
            }
        }
        assert_eq!(text, "pong");
        assert!(saw_completed);
        assert_eq!(
            *backend.thread_id.lock().unwrap(),
            Some("019fc8bb-b347-7550-87fa-e57e0c0f52df".to_string())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stub_cli_error_line_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("fake_codex_error.sh");
        write_stub_script(
            &script_path,
            r#"{"type":"thread.started","thread_id":"11111111-1111-1111-1111-111111111111"}
{"type":"turn.failed","message":"something went wrong"}"#,
        );

        let backend = CodexBackend::new(CodexConfig {
            binary_path: script_path,
            ..CodexConfig::default()
        });

        let (tx, _rx) = broadcast::channel(16);
        let result = backend
            .execute(request_with(vec![user_message("hi")]), tx, CancellationToken::new())
            .await;
        assert!(matches!(result, Err(ExecutionError::BackendError { .. })));
    }

    #[tokio::test]
    async fn execute_without_a_user_message_is_rejected_before_spawning() {
        let backend = CodexBackend::new(CodexConfig {
            binary_path: "/nonexistent/binary/should/never/be/invoked".into(),
            ..CodexConfig::default()
        });
        let (tx, _rx) = broadcast::channel(16);
        let result = backend
            .execute(request_with(vec![]), tx, CancellationToken::new())
            .await;
        assert!(matches!(result, Err(ExecutionError::InvalidRequest { .. })));
    }
}
