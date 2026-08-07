//! Subprocess `ExecutionBackend` wrapping the Claude Code CLI.
//!
//! Mirrors `harness-integration-codex`'s `backend.rs` shape: no `ModelClient`
//! / `GenericModelBackend` here — the CLI does its own tool use and context
//! management, so this backend drives it as a child process and translates
//! its stream-json output directly into [`ExecutionEvent`]s. See
//! `crates/integrations/claude-code/PLAN.md` and `wire.rs`'s module docs for
//! the wire schema.

use std::process::Stdio;
use std::sync::{Arc, Mutex};

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
use crate::wire::{
    extract_assistant_text, extract_latest_user_text, extract_result, extract_session_id,
};

/// Drives the Claude Code CLI as a subprocess `ExecutionBackend`. One
/// `claude --print` process is spawned per [`execute`](Self::execute) call;
/// continuity across calls comes from `--resume <session_id>`, threaded
/// from the `system`/`init` line of the prior call (mirrors
/// `harness-integration-codex`'s `thread_id` handling).
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
            name: "Claude Code".to_string(),
            description: "Claude Code CLI invoked as a subprocess".to_string(),
            capabilities: self.capabilities(),
        }
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            streaming: true,
            // As with Codex: tool calls (shell commands, file edits) happen
            // entirely inside the CLI subprocess's own tool loop and are
            // never relayed back to the harness.
            tool_calls: false,
            parallel_tool_calls: false,
            host_managed_tools: false,
            resumable_sessions: true,
            exact_usage: true,
            exact_cost: true,
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
                message: "no user message to send to the Claude Code CLI".to_string(),
            }
        })?;

        let resume_id = self
            .session_id
            .lock()
            .expect("session_id mutex poisoned")
            .clone();

        let mut command = tokio::process::Command::new(&self.config.binary_path);
        command
            .arg("--print")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose");

        if let Some(id) = &resume_id {
            command.arg("--resume").arg(id);
        } else if !request.system_prompt.is_empty() {
            command
                .arg("--append-system-prompt")
                .arg(&request.system_prompt);
        }

        if self.config.permission_mode != "interactive" {
            command
                .arg("--permission-mode")
                .arg(&self.config.permission_mode);
        }

        for arg in &self.config.extra_args {
            command.arg(arg);
        }

        command.arg(&prompt);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .map_err(|error| ExecutionError::BackendError {
                message: format!("failed to spawn Claude Code CLI: {error}"),
                code: "spawn_failed".to_string(),
            })?;

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

        let mut new_session_id: Option<String> = None;
        let mut sent_text = String::new();
        let mut final_result: Option<ExecutionResult> = None;
        let mut final_error: Option<String> = None;

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
                message: format!("failed to read Claude Code CLI output: {error}"),
                code: "io_error".to_string(),
            })?;
            let Some(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }

            // Tolerate stray non-JSON noise on stdout rather than failing
            // the whole run on one unparsable line.
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };

            if let Some(id) = extract_session_id(&value) {
                new_session_id = Some(id);
                continue;
            }

            if let Some(text) = extract_assistant_text(&value) {
                // Each line carries the *full* accumulated text, not a
                // delta — diff against what was already sent.
                let delta = if let Some(delta) = text.strip_prefix(sent_text.as_str()) {
                    delta.to_string()
                } else {
                    // Accumulated text diverged from what we already sent
                    // (unexpected) — resync by forwarding the full text.
                    text.clone()
                };
                if !delta.is_empty() {
                    let _ = sink.send(ExecutionEvent::TextDelta {
                        request_id: request.request_id,
                        delta,
                    });
                }
                sent_text = text;
                continue;
            }

            if let Some(result) = extract_result(&value) {
                if result.finish_reason != "success" {
                    final_error = Some(format!(
                        "Claude Code CLI reported a non-success result: {}",
                        result.finish_reason
                    ));
                }
                let cost = Cost {
                    amount_usd: result
                        .cost_usd
                        .and_then(|amount| Decimal::try_from(amount).ok()),
                    source: result.cost_usd.map(|_| CostSource::ProviderReported),
                };
                final_result = Some(ExecutionResult {
                    request_id: request.request_id,
                    usage: result.usage,
                    cost,
                    finish_reason: result.finish_reason,
                });
            }
        }

        let status = child
            .wait()
            .await
            .map_err(|error| ExecutionError::BackendError {
                message: format!("Claude Code CLI process error: {error}"),
                code: "wait_failed".to_string(),
            })?;

        if let Some(id) = new_session_id {
            *self.session_id.lock().expect("session_id mutex poisoned") = Some(id);
        }

        if let Some(message) = final_error {
            return Err(ExecutionError::BackendError {
                message,
                code: "claude_code_error".to_string(),
            });
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
                message: format!("Claude Code CLI exited ({status}) without a result line"),
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
        let config: ClaudeCodeConfig = serde_json::from_value(config)?;
        Ok(Arc::new(ClaudeCodeBackend::new(config)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::ids::{MessageId, RequestId, RunId, Timestamp};
    use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};

    #[test]
    fn backend_descriptor_is_correct() {
        let backend = ClaudeCodeBackend::new(ClaudeCodeConfig::default());
        let descriptor = backend.descriptor();
        assert_eq!(descriptor.name, "Claude Code");
        assert!(descriptor.capabilities.streaming);
        assert!(!descriptor.capabilities.tool_calls);
        assert!(!descriptor.capabilities.host_managed_tools);
        assert!(descriptor.capabilities.resumable_sessions);
    }

    #[tokio::test]
    async fn factory_constructs_backend_from_json() {
        let backend = ClaudeCodeFactory
            .create(serde_json::json!({
                "binary_path": "claude",
                "permission_mode": "bypassPermissions"
            }))
            .await
            .expect("valid Claude Code configuration");
        assert!(backend.capabilities().streaming);
        assert!(!backend.capabilities().tool_calls);
    }

    fn user_message(text: &str) -> AgentMessage {
        AgentMessage {
            id: MessageId::new(),
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
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
            params: Default::default(),
        }
    }

    #[cfg(unix)]
    fn write_stub_script(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let script = format!("#!/bin/sh\ncat <<'STUB_EOF'\n{body}\nSTUB_EOF\n");
        std::fs::write(path, script).expect("write stub script");
        let mut perms = std::fs::metadata(path)
            .expect("stat stub script")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod stub script");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stub_cli_forwards_incremental_text_and_completes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("fake_claude.sh");
        write_stub_script(
            &script_path,
            r#"{"type":"system","subtype":"init","session_id":"300a4df8-bc57-41dc-8254-76bc3dac0b7d"}
{"type":"assistant","message":{"content":[{"type":"text","text":"p"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"pong"}]}}
{"type":"result","subtype":"success","total_cost_usd":0.0149959,"usage":{"input_tokens":10,"output_tokens":44},"result":"pong"}"#,
        );

        let backend = ClaudeCodeBackend::new(ClaudeCodeConfig {
            binary_path: script_path,
            ..ClaudeCodeConfig::default()
        });

        let (tx, mut rx) = broadcast::channel(16);
        let result = backend
            .execute(
                request_with(vec![user_message("hi")]),
                tx,
                CancellationToken::new(),
            )
            .await
            .expect("execute should succeed");

        assert_eq!(result.finish_reason, "success");
        assert_eq!(result.usage.input_tokens.value(), Some(10));
        assert_eq!(result.usage.output_tokens.value(), Some(44));
        assert!(result.cost.amount_usd.is_some());

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
            *backend.session_id.lock().unwrap(),
            Some("300a4df8-bc57-41dc-8254-76bc3dac0b7d".to_string())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stub_cli_non_success_result_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("fake_claude_error.sh");
        write_stub_script(
            &script_path,
            r#"{"type":"system","subtype":"init","session_id":"11111111-1111-1111-1111-111111111111"}
{"type":"result","subtype":"error_max_turns","usage":{}}"#,
        );

        let backend = ClaudeCodeBackend::new(ClaudeCodeConfig {
            binary_path: script_path,
            ..ClaudeCodeConfig::default()
        });

        let (tx, _rx) = broadcast::channel(16);
        let result = backend
            .execute(
                request_with(vec![user_message("hi")]),
                tx,
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(result, Err(ExecutionError::BackendError { .. })));
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
