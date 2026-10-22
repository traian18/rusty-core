//! Following the active attempt: its signals while it runs, and its outcome
//! once it ends.

use harness_core::orchestration::{
    error_codes, AttemptDetails, OrchestrationCommand, OrchestrationError, OrchestrationPolicies,
    OrchestrationStatus, RetryReason, UsageSummary,
};
use serde_json::Value;
use tokio::{sync::oneshot, time::Instant};

use super::super::super::steps::StepSignal;
use super::super::{
    now_ms, step_job::StepExecution, CorrelatedAgentEvent, OrchestrationRuntimeError,
    OrchestrationUpdate,
};
use super::{ActiveStep, ApplyError, RunLoop};

type JoinedStep = Result<StepExecution, tokio::task::JoinError>;

impl RunLoop {
    pub(super) async fn on_signal(
        &mut self,
        signal: StepSignal,
    ) -> Result<(), OrchestrationRuntimeError> {
        let Some(active) = self.active.as_mut() else {
            return Ok(());
        };
        active.last_activity = Instant::now();
        let (node_id, attempt) = (active.node_id.clone(), active.attempt);
        match signal {
            StepSignal::Checkpoint { value, committed } => {
                self.record_checkpoint(value, committed).await
            }
            StepSignal::Delegated(delegated) => {
                active.delegated = Some(delegated);
                Ok(())
            }
            StepSignal::Usage(usage) => {
                let exceeded = budget_error(
                    &self.runner.compiled.definition.policies,
                    &self.state.usage,
                    &usage,
                );
                active.usage = usage;
                self.abort_on(exceeded).await
            }
            StepSignal::Agent(envelope) => {
                active.actions.observe(&envelope.event);
                let correlation = self.correlation(self.active.as_ref().expect("active"));
                self.publish(OrchestrationUpdate::Agent(Box::new(CorrelatedAgentEvent {
                    correlation,
                    envelope: *envelope,
                })));
                Ok(())
            }
            StepSignal::PermissionRequested { permission_id, .. } => {
                let command = OrchestrationCommand::PermissionRequired {
                    node_id,
                    attempt,
                    permission_id,
                };
                self.apply_or_warn(command, "ignored permission request")
                    .await
            }
            StepSignal::InputRequested(request) => {
                let command = OrchestrationCommand::InputRequired {
                    node_id,
                    attempt,
                    request,
                };
                self.apply_or_warn(command, "ignored question").await
            }
        }
    }

    async fn record_checkpoint(
        &mut self,
        value: Value,
        committed: oneshot::Sender<Result<(), String>>,
    ) -> Result<(), OrchestrationRuntimeError> {
        let active = self
            .active
            .as_ref()
            .expect("checkpoint from the active step");
        let command = OrchestrationCommand::RecordCheckpoint {
            node_id: active.node_id.clone(),
            attempt: active.attempt,
            value,
        };
        let result = self.apply(command).await;
        // The step may have stopped waiting (it was cancelled); the run's own
        // outcome below is what matters then.
        let _ = committed.send(result.as_ref().map(|_| ()).map_err(ToString::to_string));
        result
    }

    /// A step's request the reducer refuses is logged and the step keeps
    /// running; only a persistence failure ends the run.
    async fn apply_or_warn(
        &mut self,
        command: OrchestrationCommand,
        what: &'static str,
    ) -> Result<(), OrchestrationRuntimeError> {
        match self.try_apply(command).await {
            Ok(()) => Ok(()),
            Err(ApplyError::Rejected(error)) => {
                tracing::warn!(code = error.code, %error, "{what}");
                Ok(())
            }
            Err(ApplyError::Fatal(error)) => Err(error),
        }
    }

    async fn abort_on(
        &mut self,
        error: Option<OrchestrationError>,
    ) -> Result<(), OrchestrationRuntimeError> {
        match error {
            Some(error) => self.apply(OrchestrationCommand::Abort { error }).await,
            None => Ok(()),
        }
    }

    pub(super) async fn on_step_done(
        &mut self,
        joined: JoinedStep,
    ) -> Result<(), OrchestrationRuntimeError> {
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        self.drain_final_signals(&mut active).await?;
        // Cancelled or aborted while running: the reducer already recorded
        // the attempt as cancelled.
        if self.state.status.is_terminal() || self.state.status == OrchestrationStatus::Cancelling {
            return Ok(());
        }
        let execution = self.attempt_execution(joined, &active);
        self.apply(finished_attempt_command(active, execution))
            .await
    }

    /// Observations sent just before the executor returned.
    async fn drain_final_signals(
        &mut self,
        active: &mut ActiveStep,
    ) -> Result<(), OrchestrationRuntimeError> {
        while let Ok(signal) = active.signals.try_recv() {
            match signal {
                StepSignal::Delegated(delegated) => active.delegated = Some(delegated),
                StepSignal::Usage(usage) => {
                    let exceeded = budget_error(
                        &self.runner.compiled.definition.policies,
                        &self.state.usage,
                        &usage,
                    );
                    active.usage = usage;
                    self.abort_on(exceeded).await?;
                }
                StepSignal::Checkpoint { committed, .. } => {
                    // The attempt is over, so the checkpoint is never
                    // committed; the step side has usually gone already.
                    let _ = committed.send(Err("attempt ended before checkpoint commit".into()));
                }
                StepSignal::Agent(envelope) => active.actions.observe(&envelope.event),
                StepSignal::PermissionRequested { .. } | StepSignal::InputRequested(_) => {}
            }
        }
        Ok(())
    }

    /// The attempt's outcome; a panic or a watchdog stall replaces it.
    fn attempt_execution(&self, joined: JoinedStep, active: &ActiveStep) -> StepExecution {
        let mut execution = joined.unwrap_or_else(|error| StepExecution {
            result: Err(OrchestrationError::new(
                "step_panicked",
                format!("step executor terminated abnormally: {error}"),
            )),
            details: AttemptDetails::default(),
        });
        if active.stalled {
            let limit = self
                .runner
                .compiled
                .definition
                .policies
                .stall_timeout_ms
                .unwrap_or_default();
            execution.result = Err(OrchestrationError::retryable(
                "step_stalled",
                format!(
                    "step {} made no progress for {limit}ms and was cancelled",
                    active.node_id
                ),
                RetryReason::BackendTimeout,
            ));
        }
        execution
    }
}

fn finished_attempt_command(active: ActiveStep, execution: StepExecution) -> OrchestrationCommand {
    let mut details = execution.details;
    details.started_at_ms = Some(active.started_at_ms);
    details.ended_at_ms = Some(now_ms());
    details.delegated = active.delegated;
    details.usage = active.usage;
    details.actions = active.actions.finish();
    match execution.result {
        Ok(output) => OrchestrationCommand::StepSucceeded {
            node_id: active.node_id,
            attempt: active.attempt,
            output,
            details,
        },
        Err(error) => OrchestrationCommand::StepFailed {
            node_id: active.node_id,
            attempt: active.attempt,
            error,
            details,
        },
    }
}

/// The first run budget that the active attempt's usage pushes over, as the
/// error to abort the run with.
fn budget_error(
    policies: &OrchestrationPolicies,
    run_usage: &UsageSummary,
    attempt_usage: &UsageSummary,
) -> Option<OrchestrationError> {
    let mut total = run_usage.clone();
    total.add(attempt_usage);
    let exhausted = [
        (
            "model request",
            policies.max_model_requests,
            total.model_requests,
        ),
        ("tool call", policies.max_tool_calls, total.tool_calls),
        ("token", policies.max_tokens, total.tokens),
    ]
    .into_iter()
    .find_map(|(name, limit, used)| {
        limit
            .filter(|limit| used >= *limit)
            .map(|limit| format!("{name} budget of {limit} exhausted ({used} used)"))
    });
    let message = exhausted.or_else(|| {
        policies
            .max_cost_usd
            .filter(|limit| total.cost_usd >= *limit)
            .map(|limit| format!("cost budget of ${limit} exhausted"))
    })?;
    Some(OrchestrationError::new(
        error_codes::BUDGET_EXHAUSTED,
        message,
    ))
}
