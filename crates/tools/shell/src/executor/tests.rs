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
    let stdout = result.output["stdout"]
        .as_str()
        .expect("stdout is a string");
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
