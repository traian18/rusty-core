use std::process::Stdio;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tracing::{info, warn};

use harness_tools::{
    CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult,
};

/// Input for the `shell.exec` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ExecInput {
    /// Executable to run.
    pub command: String,
    /// Arguments passed directly to the executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory. Defaults to the current process directory.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Maximum execution time in seconds.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Executes a process, captures stdout/stderr, and terminates it on timeout or
/// cancellation.
#[derive(Clone, Default)]
pub struct ExecTool;

impl ExecTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolExecutor for ExecTool {
    fn descriptor(&self) -> ToolDescriptor {
        let schema = schemars::schema_for!(ExecInput);
        ToolDescriptor {
            id: ToolId::new("shell.exec"),
            name: "Execute shell command".to_string(),
            description: "Execute a command and capture its output".to_string(),
            input_schema: serde_json::to_value(schema).unwrap_or_else(|_| json!({})),
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let input: ExecInput = input.parse().map_err(|_| ToolError::ExecutionFailed)?;

        if cancel.is_cancelled() {
            return Err(ToolError::Timeout);
        }

        info!(command = %input.command, args = ?input.args, "shell.exec: running command");

        let mut command = tokio::process::Command::new(&input.command);
        command
            .args(&input.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &input.cwd {
            command.current_dir(cwd);
        }

        let mut child = command.spawn().map_err(|error| {
            warn!(%error, "shell.exec: spawn failed");
            ToolError::ExecutionFailed
        })?;
        let stdout = child.stdout.take().ok_or(ToolError::Internal)?;
        let stderr = child.stderr.take().ok_or(ToolError::Internal)?;

        let stdout_task = tokio::spawn(read_stream(stdout));
        let stderr_task = tokio::spawn(read_stream(stderr));

        enum Completion {
            Exited(std::io::Result<std::process::ExitStatus>),
            Cancelled,
            TimedOut,
        }

        let timeout = async {
            match input.timeout_secs {
                Some(seconds) => tokio::time::sleep(std::time::Duration::from_secs(seconds)).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(timeout);

        let completion = tokio::select! {
            status = child.wait() => Completion::Exited(status),
            _ = cancel.cancelled() => Completion::Cancelled,
            _ = &mut timeout => Completion::TimedOut,
        };

        if matches!(completion, Completion::Cancelled | Completion::TimedOut) {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }

        let stdout = stdout_task.await.map_err(|_| ToolError::Internal)?;
        let stderr = stderr_task.await.map_err(|_| ToolError::Internal)?;

        match completion {
            Completion::Cancelled | Completion::TimedOut => Err(ToolError::Timeout),
            Completion::Exited(Ok(status)) if status.success() => Ok(ToolResult {
                call_id: "shell.exec".to_string(),
                output: json!({
                    "stdout": stdout,
                    "stderr": stderr,
                    "exit_code": status.code().unwrap_or(0)
                }),
                is_error: false,
            }),
            Completion::Exited(Ok(status)) => Ok(ToolResult {
                call_id: "shell.exec".to_string(),
                output: json!({
                    "stdout": stdout,
                    "stderr": stderr,
                    "exit_code": status.code().unwrap_or(-1)
                }),
                is_error: true,
            }),
            Completion::Exited(Err(error)) => {
                warn!(%error, "shell.exec: failed while waiting for command");
                Err(ToolError::ExecutionFailed)
            }
        }
    }
}

async fn read_stream<R>(stream: R) -> String
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = tokio::io::BufReader::new(stream);
    let mut output = String::new();
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => output.push_str(&line),
            Err(error) => {
                warn!(%error, "shell.exec: failed while reading process output");
                break;
            }
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn executes_a_real_process() {
        let result = ExecTool::new()
            .execute(
                ToolInput {
                    arguments: json!({
                        "command": "sh",
                        "args": ["-c", "printf phase4"]
                    }),
                },
                CancellationToken::new(),
            )
            .await
            .expect("command should execute");

        assert!(!result.is_error);
        assert_eq!(result.output["stdout"], "phase4");
        assert_eq!(result.output["exit_code"], 0);
    }

    #[tokio::test]
    async fn cancellation_terminates_the_process_promptly() {
        let cancel = CancellationToken::new();
        let cancel_trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_trigger.cancel();
        });

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            ExecTool::new().execute(
                ToolInput {
                    arguments: json!({
                        "command": "sh",
                        "args": ["-c", "sleep 30"]
                    }),
                },
                cancel,
            ),
        )
        .await
        .expect("cancelled command should not remain alive");

        assert!(matches!(result, Err(ToolError::Timeout)));
    }

    #[tokio::test]
    async fn timeout_terminates_the_process() {
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            ExecTool::new().execute(
                ToolInput {
                    arguments: json!({
                        "command": "sh",
                        "args": ["-c", "sleep 30"],
                        "timeout_secs": 1
                    }),
                },
                CancellationToken::new(),
            ),
        )
        .await
        .expect("timed-out command should not remain alive");

        assert!(matches!(result, Err(ToolError::Timeout)));
    }
}
