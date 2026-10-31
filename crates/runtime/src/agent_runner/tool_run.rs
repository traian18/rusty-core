//! Running one registered tool call off the agent loop and reporting its
//! result to the agent's mailbox.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, OwnedSemaphorePermit};
use tokio_util::sync::CancellationToken;

use rusty_protocol::commands::AgentCommand;
use rusty_protocol::ids::ToolCallId;
use rusty_tool_api::ToolExecutor;

use super::mailbox::deliver;
use crate::scheduler::Scheduler;

/// What one tool call runs with, off the agent loop.
pub(super) struct ToolRun {
    pub(super) call_id: ToolCallId,
    pub(super) name: String,
    pub(super) arguments: serde_json::Value,
    /// `None` when no registered tool has this name.
    pub(super) executor: Option<Arc<dyn ToolExecutor>>,
    pub(super) scheduler: Arc<Scheduler>,
    pub(super) token: CancellationToken,
    pub(super) commands: mpsc::Sender<AgentCommand>,
    pub(super) session_id: String,
}

impl ToolRun {
    pub(super) async fn run(self) {
        let timeout = self.timeout();
        let Some(_permits) = self.acquire_permits().await else {
            return;
        };
        let ToolRun {
            call_id,
            name,
            arguments,
            executor,
            token,
            commands,
            session_id,
            ..
        } = self;
        let Some(executor) = executor else {
            let error = rusty_protocol::tools::ToolError::ExecutionFailed;
            deliver(&commands, AgentCommand::ToolFailed { call_id, error }).await;
            return;
        };

        let started = Instant::now();
        // Cancelling the token (not just dropping the future) lets the
        // executor reap what it started, e.g. a shell process group.
        let timeout_token = token.clone();
        let execution = rusty_tool_api::with_tool_call(
            call_id.to_string(),
            session_id,
            executor.execute(rusty_tool_api::ToolInput { arguments }, token),
        );
        let outcome = match timeout {
            Some(limit) => tokio::time::timeout(limit, execution)
                .await
                .unwrap_or_else(|_| timed_out(&name, timeout, &timeout_token)),
            None => execution.await,
        };
        record_call(&name, started, outcome.is_ok());
        deliver(&commands, report(call_id, outcome)).await;
    }

    fn timeout(&self) -> Option<Duration> {
        let default = self.scheduler.tool_call_timeout();
        self.executor.as_ref().map_or(Some(default), |executor| {
            executor.call_timeout(&self.arguments, default)
        })
    }

    /// The generic tool-execution permit, plus a process permit for a tool
    /// that forks one. `None` when the call is cancelled while waiting.
    async fn acquire_permits(
        &self,
    ) -> Option<(OwnedSemaphorePermit, Option<OwnedSemaphorePermit>)> {
        // Like provider capacity, tool capacity is a cancellable queue
        // wait and must not stall the runner's command mailbox.
        let permit = tokio::select! {
            permit = self.scheduler.acquire_tool_permit() => permit,
            _ = self.token.cancelled() => return None,
        };
        let process_permit = if self.spawns_process() {
            Some(
                self.scheduler
                    .acquire_process_permit_cancellable(&self.token)
                    .await?,
            )
        } else {
            None
        };
        Some((permit, process_permit))
    }

    /// M3: `shell.exec` (and any future tool with the same shape) forks a
    /// real OS process, which is a heavier resource than "a tool call" in
    /// general — bound it separately via `max_concurrent_processes` in
    /// addition to the generic tool-execution permit, so a burst of shell
    /// calls can't spawn unboundedly many processes even while comfortably
    /// inside `max_concurrent_tool_executions`.
    fn spawns_process(&self) -> bool {
        self.name == "shell.exec"
    }
}

fn timed_out(
    name: &str,
    timeout: Option<Duration>,
    token: &CancellationToken,
) -> Result<rusty_tool_api::ToolResult, rusty_tool_api::ToolError> {
    token.cancel();
    tracing::warn!(tool = %name, ?timeout, "tool call timed out");
    metrics::counter!(
        "harness_tool_call_timeouts_total",
        "tool" => name.to_owned()
    )
    .increment(1);
    Err(rusty_tool_api::ToolError::Timeout)
}

fn record_call(name: &str, started: Instant, succeeded: bool) {
    metrics::histogram!("harness_tool_call_duration_seconds", "tool" => name.to_owned())
        .record(started.elapsed().as_secs_f64());
    metrics::counter!(
        "harness_tool_calls_total",
        "tool" => name.to_owned(),
        "outcome" => if succeeded { "success" } else { "error" }
    )
    .increment(1);
}

fn report(
    call_id: ToolCallId,
    outcome: Result<rusty_tool_api::ToolResult, rusty_tool_api::ToolError>,
) -> AgentCommand {
    match outcome {
        Ok(tool_result) => AgentCommand::ToolCompleted {
            call_id,
            result: rusty_protocol::tools::ToolResult {
                call_id,
                output: tool_result.output,
                is_error: tool_result.is_error,
            },
        },
        Err(tool_error) => AgentCommand::ToolFailed {
            call_id,
            error: to_protocol_tool_error(tool_error),
        },
    }
}

fn to_protocol_tool_error(error: rusty_tool_api::ToolError) -> rusty_protocol::tools::ToolError {
    match error {
        rusty_tool_api::ToolError::ExecutionFailed => {
            rusty_protocol::tools::ToolError::ExecutionFailed
        }
        rusty_tool_api::ToolError::PermissionDenied => {
            rusty_protocol::tools::ToolError::PermissionDenied
        }
        rusty_tool_api::ToolError::Timeout => rusty_protocol::tools::ToolError::Timeout,
        rusty_tool_api::ToolError::Internal => rusty_protocol::tools::ToolError::Internal,
    }
}
