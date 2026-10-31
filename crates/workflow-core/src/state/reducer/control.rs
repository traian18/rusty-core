//! Pausing, cancelling and recovering a run.

use crate::compiler::CompiledOrchestration;
use crate::definition::{OrchestrationNodeId, OrchestrationNodeKind};

use super::finish::terminate_failed;
use super::lookup::require_status;
use super::lookup::transition_error;
use super::lookup::{current_attempt, step_state};
use super::queries::check_restorable;
use crate::state::{
    error_codes, AttemptStatus, OrchestrationEffect, OrchestrationError, OrchestrationEvent,
    OrchestrationOutcome, OrchestrationResult, OrchestrationRunState, OrchestrationStatus,
    StepStatus, TransitionError,
};

pub(super) fn pause(
    state: &mut OrchestrationRunState,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    if state.paused {
        return Err(transition_error("already_paused", "run is already paused"));
    }
    if matches!(
        state.status,
        OrchestrationStatus::Created | OrchestrationStatus::Cancelling
    ) {
        return Err(transition_error(
            "invalid_run_status",
            format!("cannot pause a run in status {:?}", state.status),
        ));
    }
    state.paused = true;
    settle(state);
    Ok(vec![OrchestrationEffect::Emit(
        OrchestrationEvent::RunPaused,
    )])
}

pub(super) fn resume(
    state: &mut OrchestrationRunState,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    if !state.paused {
        return Err(transition_error("not_paused", "run is not paused"));
    }
    state.paused = false;
    settle(state);
    Ok(vec![OrchestrationEffect::Emit(
        OrchestrationEvent::RunResumed,
    )])
}

pub(super) fn abort(
    state: &mut OrchestrationRunState,
    error: OrchestrationError,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let mut effects = Vec::new();
    for (node_id, step) in &mut state.steps {
        if step.status.is_active() {
            let attempt = current_attempt(step)?;
            attempt.status = AttemptStatus::Cancelled;
            effects.push(OrchestrationEffect::CancelStep {
                node_id: node_id.clone(),
                attempt: attempt.attempt,
            });
            step.status = StepStatus::Cancelled;
            step.pending_permissions.clear();
            step.pending_input = None;
        }
    }
    terminate_failed(state, error, effects)
}

pub(super) fn cancel(
    state: &mut OrchestrationRunState,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    if state.status == OrchestrationStatus::Cancelling {
        return Err(transition_error(
            "already_cancelling",
            "run is already cancelling",
        ));
    }
    state.status = OrchestrationStatus::Cancelling;
    let mut effects = Vec::new();
    for (node_id, step) in &mut state.steps {
        match step.status {
            StepStatus::Running
            | StepStatus::WaitingForPermission
            | StepStatus::WaitingForInput => {
                let attempt = current_attempt(step)?;
                attempt.status = AttemptStatus::Cancelled;
                effects.push(OrchestrationEffect::CancelStep {
                    node_id: node_id.clone(),
                    attempt: attempt.attempt,
                });
                step.status = StepStatus::Cancelled;
                step.pending_permissions.clear();
                step.pending_input = None;
            }
            StepStatus::Ready | StepStatus::RetryScheduled => step.status = StepStatus::Cancelled,
            StepStatus::Pending => step.status = StepStatus::Skipped,
            _ => {}
        }
    }
    if effects.is_empty() {
        return cancellation_completed(state);
    }
    Ok(effects)
}

pub(super) fn cancellation_completed(
    state: &mut OrchestrationRunState,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    require_status(state.status, OrchestrationStatus::Cancelling)?;
    state.status = OrchestrationStatus::Cancelled;
    Ok(vec![
        OrchestrationEffect::Emit(OrchestrationEvent::RunCancelled),
        OrchestrationEffect::Finish(OrchestrationResult::from_state(
            state,
            OrchestrationOutcome::Cancelled,
        )),
    ])
}

pub(super) fn recover(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    check_restorable(compiled, state)?;
    let mut effects = vec![OrchestrationEffect::Emit(OrchestrationEvent::RunRestored)];
    if state.status == OrchestrationStatus::Cancelling {
        for step in state.steps.values_mut() {
            if step.status.is_active() {
                step.status = StepStatus::Cancelled;
            }
        }
        effects.extend(cancellation_completed(state)?);
        return Ok(effects);
    }
    // Input, verify, approval and output nodes are pure functions of
    // persisted state, so an interrupted attempt is simply abandoned and the
    // step re-admitted (an unanswered approval is asked again). Only agent
    // attempts have side effects of unknown extent.
    let (indeterminate, replayable): (Vec<_>, Vec<_>) = state
        .steps
        .iter()
        .filter(|(_, step)| step.status.is_active())
        .map(|(id, _)| id.clone())
        .partition(|id| {
            matches!(
                compiled.node(id).map(|node| &node.kind),
                Some(OrchestrationNodeKind::Agent(_) | OrchestrationNodeKind::Subflow(_))
            )
        });
    for node_id in &replayable {
        let step = step_state(state, node_id)?;
        let attempt = current_attempt(step)?;
        attempt.status = AttemptStatus::Cancelled;
        step.status = StepStatus::Ready;
        step.pending_permissions.clear();
        step.pending_input = None;
        effects.push(OrchestrationEffect::Emit(OrchestrationEvent::StepReady {
            node_id: node_id.clone(),
        }));
    }
    if indeterminate.is_empty() {
        settle(state);
        return Ok(effects);
    }
    let error = OrchestrationError::new(
        error_codes::INDETERMINATE_ATTEMPT,
        format!(
            "step(s) {} were in flight when the run stopped; side effects are unknown",
            indeterminate
                .iter()
                .map(OrchestrationNodeId::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    for node_id in &indeterminate {
        let step = step_state(state, node_id)?;
        let attempt = current_attempt(step)?;
        attempt.status = AttemptStatus::Failed;
        attempt.error = Some(error.clone());
        step.status = StepStatus::Failed;
        step.error = Some(error.clone());
        step.pending_permissions.clear();
        step.pending_input = None;
    }
    terminate_failed(state, error, effects)
}

/// Derive the non-terminal run status from step states and the pause flag.
pub(super) fn settle(state: &mut OrchestrationRunState) {
    if state.status.is_terminal() || state.status == OrchestrationStatus::Cancelling {
        return;
    }
    state.status = match state.active_step().map(|(_, step)| step.status) {
        Some(StepStatus::WaitingForPermission) => OrchestrationStatus::WaitingForPermission,
        Some(StepStatus::WaitingForInput) => OrchestrationStatus::WaitingForInput,
        Some(_) => OrchestrationStatus::Running,
        None if state.paused => OrchestrationStatus::Paused,
        None => OrchestrationStatus::Ready,
    };
}
