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
        // M3: put the child in its own process group so cancellation/timeout
        // can terminate the whole tree it spawns (e.g. `sh -c "cmd &"`
        // backgrounding a grandchild), not just the direct child PID that
        // `Child::start_kill()` alone would signal.
        #[cfg(unix)]
        {
            command.process_group(0);
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
            kill_process_tree(&mut child);
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

/// Terminates `child` and, on Unix, its entire process group — not just the
/// direct child PID. `spawn` puts the child in its own process group (see
/// `command.process_group(0)` above), so sending `SIGKILL` to the negated
/// PID reaches every descendant the child forked (e.g. a `sh -c "cmd &"`
/// background job), preventing the kind of orphaned-grandchild leak that
/// `Child::start_kill()` alone (which only signals the direct child) cannot
/// prevent.
///
/// On non-Unix platforms this falls back to killing only the direct child;
/// process-tree termination there is a known gap (no `CREATE_NEW_PROCESS_GROUP`
/// / job-object wiring yet).
fn kill_process_tree(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            // Negative PID targets the whole process group that
            // `command.process_group(0)` created at spawn time.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
    let _ = child.start_kill();
}

/// Per-stream (stdout/stderr) output cap. Mirrors the truncation pattern
/// used by `harness-tool-git`'s `MAX_DIFF_BYTES` and `harness-tool-web`'s
/// `read_capped`: a captured tool result must not grow unbounded just
/// because the child process is chatty or adversarial.
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

async fn read_stream<R>(stream: R) -> String
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = tokio::io::BufReader::new(stream);
    let mut output = String::new();
    let mut line = String::new();
    let mut truncated = false;

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                // Keep reading to EOF even after the cap is hit: the pipe
                // must be drained or a chatty child can block writing to a
                // full OS pipe buffer and never reach the point where it
                // observes cancellation/timeout, i.e. a full read buffer
                // could otherwise turn an output-size problem into a hang.
                if !truncated {
                    if output.len() + line.len() > MAX_OUTPUT_BYTES {
                        output.push_str("\n... (output truncated)");
                        truncated = true;
                    } else {
                        output.push_str(&line);
                    }
                }
            }
            Err(error) => {
                warn!(%error, "shell.exec: failed while reading process output");
                break;
            }
        }
    }

    output
}

#[cfg(test)]
mod tests;
