//! A check that runs a shell command and reads its exit code like a hook.

use rusty_protocol::commands::VerdictOutcome;
use rusty_protocol::effects::CompletionEvaluationRequest;

use serde_json::{json, Value};

use tokio_util::sync::CancellationToken;

use super::{GateContext, MAX_FEEDBACK_CHARS};

use crate::completion_gate::text::{render_transcript, truncate};

pub(super) fn shell(command: &str) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut shell = tokio::process::Command::new("cmd");
        shell.arg("/C").arg(command);
        shell
    }
    #[cfg(not(windows))]
    {
        let mut shell = tokio::process::Command::new("sh");
        shell.arg("-c").arg(command);
        shell
    }
}

/// Run a trusted hook command. Input is the `Stop` hook JSON on stdin; the
/// command's exit code and stdout decide the verdict.
pub(super) async fn run_command(
    context: &GateContext,
    request: &CompletionEvaluationRequest,
    command: &str,
    timeout_ms: u64,
    cancel: &CancellationToken,
) -> VerdictOutcome {
    if !context.commands_trusted {
        return VerdictOutcome::Error {
            message: "command evaluators are not trusted for this agent; the host must allow them"
                .into(),
        };
    }
    let root = context.workspace.root().to_path_buf();
    let input = json!({
        "hook_event_name": "Stop",
        "cwd": root,
        "run_id": request.run_id.to_string(),
        "attempt": request.attempt,
        // Claude Code's flag for "already continuing because of a Stop hook".
        "stop_hook_active": request.attempt > 1,
        "last_assistant_message": request.final_response,
        "transcript": render_transcript(&request.transcript),
    });
    let mut process = shell(command);
    process
        .current_dir(&root)
        .env("RUSTY_PROJECT_DIR", &root)
        .env("CLAUDE_PROJECT_DIR", &root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            return VerdictOutcome::Error {
                message: format!("could not start command: {error}"),
            }
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(input.to_string().as_bytes()).await;
    }
    let output = tokio::select! {
        output = child.wait_with_output() => output,
        _ = tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)) => {
            return VerdictOutcome::Error { message: format!("command timed out after {timeout_ms}ms") };
        }
        _ = cancel.cancelled() => {
            return VerdictOutcome::Error { message: "evaluation cancelled".into() };
        }
    };
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            return VerdictOutcome::Error {
                message: format!("command failed: {error}"),
            }
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    command_verdict(output.status.code(), &stdout, &stderr)
}

/// Exit 0 passes unless stdout blocks; exit 2 fails with stderr; anything
/// else is an error (the check's `error_policy` decides).
pub(super) fn command_verdict(code: Option<i32>, stdout: &str, stderr: &str) -> VerdictOutcome {
    match code {
        Some(0) => {
            let decision: Option<Value> = serde_json::from_str(stdout).ok();
            match decision {
                Some(value) if value.get("decision").and_then(Value::as_str) == Some("block") => {
                    VerdictOutcome::Fail {
                        feedback: truncate(
                            value
                                .get("reason")
                                .and_then(Value::as_str)
                                .filter(|reason| !reason.trim().is_empty())
                                .unwrap_or("Blocked by a completion hook."),
                            MAX_FEEDBACK_CHARS,
                        ),
                    }
                }
                _ => VerdictOutcome::Pass,
            }
        }
        Some(2) => VerdictOutcome::Fail {
            feedback: if stderr.is_empty() {
                "Blocked by a completion hook.".into()
            } else {
                truncate(stderr, MAX_FEEDBACK_CHARS)
            },
        },
        other => VerdictOutcome::Error {
            message: format!(
                "command exited with {}: {}",
                other.map_or("a signal".to_string(), |code| format!("code {code}")),
                truncate(stderr, 500)
            ),
        },
    }
}

// ---------------------------------------------------------------------------
// Agent evaluator: an isolated verifier session
// ---------------------------------------------------------------------------
