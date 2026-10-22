//! Explicit user retry of a failed run from its failed-step checkpoint.

use harness_core::orchestration::{
    check_restorable, OrchestrationCommand, OrchestrationError, OrchestrationNodeId,
    OrchestrationNodeKind, OrchestrationRunId, OrchestrationRunState, OrchestrationStatus, StepRun,
    StepStatus,
};
use tokio_util::sync::CancellationToken;

use super::{OrchestrationHandle, OrchestrationRunner, OrchestrationRuntimeError};

impl OrchestrationRunner {
    /// Explicit user retry from a failed checkpoint. Preserve outputs and attempt
    /// history; never replay successful steps or silently change definitions.
    pub fn retry_failed(
        &self,
        mut state: OrchestrationRunState,
        run_id: OrchestrationRunId,
        guidance: String,
    ) -> Result<OrchestrationHandle, OrchestrationRuntimeError> {
        check_restorable(&self.compiled, &state)?;
        self.check_tool_references()?;
        let node_id = take_failed_checkpoint(&mut state)?;
        let repair_target = self.repair_target(&state, &node_id);
        let error = state
            .steps
            .get_mut(&node_id)
            .and_then(|step| step.error.take());
        let resumed_node = match repair_target {
            Some(target_id) => {
                self.reset_retry_span(&mut state, &node_id, &target_id);
                target_id
            }
            None => node_id,
        };
        let step = state
            .steps
            .get_mut(&resumed_node)
            .expect("resumed step exists");
        continue_step(step, error, &guidance);
        reopen_run(&mut state, run_id);
        Ok(self.spawn(
            state,
            0,
            0,
            OrchestrationCommand::Recover,
            CancellationToken::new(),
        ))
    }

    /// A verify step that failed its check sends the work back to its repair
    /// target, if it has one.
    fn repair_target(
        &self,
        state: &OrchestrationRunState,
        node_id: &OrchestrationNodeId,
    ) -> Option<OrchestrationNodeId> {
        let verification_failed = state
            .error
            .as_ref()
            .is_some_and(|error| error.code == "verification_failed");
        match self.compiled.node(node_id).map(|node| &node.kind) {
            Some(OrchestrationNodeKind::Verify(config)) if verification_failed => {
                config.retry_target.clone()
            }
            _ => None,
        }
    }

    /// An explicit continuation sends the work back to the repair target after
    /// its automatic retries have run out. Clear downstream results so the next
    /// review checks the repaired workspace, not stale output.
    fn reset_retry_span(
        &self,
        state: &mut OrchestrationRunState,
        node_id: &OrchestrationNodeId,
        target_id: &OrchestrationNodeId,
    ) {
        for span_node in &self.compiled.retry_spans[node_id] {
            let step = state
                .steps
                .get_mut(span_node)
                .expect("retry span step exists");
            step.status = StepStatus::Pending;
            step.output = None;
            step.pending_permissions.clear();
            step.pending_input = None;
            if span_node != target_id {
                step.checkpoint = None;
            }
        }
    }
}

/// The failed step to retry, taken from a failed run's checkpoint.
fn take_failed_checkpoint(
    state: &mut OrchestrationRunState,
) -> Result<OrchestrationNodeId, OrchestrationRuntimeError> {
    if state.status != OrchestrationStatus::Failed {
        return Err(restore_error("only failed runs can be retried"));
    }
    let node_id = state
        .failed_step
        .take()
        .ok_or_else(|| restore_error("no failed step checkpoint is available"))?;
    let step = state
        .steps
        .get(&node_id)
        .ok_or_else(|| restore_error("failed step is missing"))?;
    if step.status != StepStatus::Failed {
        return Err(restore_error("checkpoint step is not failed"));
    }
    Ok(node_id)
}

fn restore_error(message: &str) -> OrchestrationRuntimeError {
    OrchestrationRuntimeError::Restore(message.into())
}

/// Readies the resumed step with the failure and the user's guidance as
/// feedback.
fn continue_step(step: &mut StepRun, error: Option<OrchestrationError>, guidance: &str) {
    if let Some(error) = error {
        step.feedback.push(error);
    }
    // The user chose to continue: the step's retries count afresh from
    // here, so a rejected repair can be sent back again instead of
    // failing the run after one more attempt.
    step.retry_base = step.attempts.len() as u32;
    step.feedback.push(OrchestrationError::new("USER_CONTINUATION", format!(
        "Resume this step. Earlier successful steps and their outputs are retained. The failed attempt may have changed the workspace: inspect current state before repeating actions. User guidance: {guidance}"
    )));
    step.status = StepStatus::Ready;
    step.pending_permissions.clear();
    step.pending_input = None;
}

/// Puts the run back into a runnable state under its new id.
fn reopen_run(state: &mut OrchestrationRunState, run_id: OrchestrationRunId) {
    for step in state.steps.values_mut() {
        if step.status == StepStatus::Skipped {
            step.status = StepStatus::Pending;
        }
    }
    state.run_id = run_id;
    state.status = OrchestrationStatus::Ready;
    state.paused = false;
    state.error = None;
    state.final_output = None;
}
