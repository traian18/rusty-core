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

    /// M3: `child.start_kill()` alone only signals the direct child PID. A
    /// shell command that forks a grandchild (e.g. `sh -c "sleep 30 &
    /// wait"`, where the backgrounded `sleep` is a separate process not
    /// directly awaited by the killed `sh`) must not leak that grandchild
    /// past cancellation — the whole process *tree* must terminate.
    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_terminates_grandchild_processes_too() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("grandchild.pid");
        let marker_arg = marker.to_string_lossy().to_string();

        let cancel = CancellationToken::new();
        let cancel_trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel_trigger.cancel();
        });

        // The shell backgrounds a grandchild `sleep`, records its PID to
        // `marker` before sleeping, and then `wait`s on it. If cancellation
        // only killed the direct `sh` PID (not its whole process group), the
        // backgrounded `sleep` would become an orphan and keep running.
        let script = format!("sleep 30 & echo $! > {marker_arg}; wait");

        let result = tokio::time::timeout(
            Duration::from_secs(3),
            ExecTool::new().execute(
                ToolInput {
                    arguments: json!({
                        "command": "sh",
                        "args": ["-c", script]
                    }),
                },
                cancel,
            ),
        )
        .await
        .expect("cancelled command should not remain alive");
        assert!(matches!(result, Err(ToolError::Timeout)));

        // Give the OS a moment to actually reap the process, then check the
        // grandchild's PID (written before it slept) is no longer alive.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let grandchild_pid: i32 = tokio::fs::read_to_string(&marker)
            .await
            .expect("grandchild should have recorded its PID before sleeping")
            .trim()
            .parse()
            .expect("PID should be a valid integer");

        // Signal 0 checks liveness without actually sending a signal.
        let still_alive = unsafe { libc::kill(grandchild_pid, 0) } == 0;
        assert!(
            !still_alive,
            "grandchild process {grandchild_pid} must not survive cancellation of its parent shell"
        );
    }

    /// M3: a chatty (or adversarial) command must not grow the captured
    /// output unbounded — it should be truncated at `MAX_OUTPUT_BYTES` with
    /// a clear marker, not silently OOM the host process.
    #[tokio::test]
    async fn output_is_capped_and_marked_as_truncated() {
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            ExecTool::new().execute(
                ToolInput {
                    arguments: json!({
                        "command": "sh",
                        // Print well beyond the 1MB cap (5 bytes/line × 400k
                        // lines ≈ 2MB).
                        "args": ["-c", "yes line | head -n 400000"]
                    }),
                },
                CancellationToken::new(),
            ),
        )
        .await
        .expect("command should not hang")
        .expect("command should execute");

        assert!(!result.is_error);
        let stdout = result.output["stdout"].as_str().expect("stdout is a string");
        assert!(
            stdout.len() <= MAX_OUTPUT_BYTES + "\n... (output truncated)".len(),
            "captured stdout must not exceed the cap plus the truncation marker, got {} bytes",
            stdout.len()
        );
        assert!(
            stdout.ends_with("... (output truncated)"),
            "truncated output must carry an explicit marker"
        );
    }
}
