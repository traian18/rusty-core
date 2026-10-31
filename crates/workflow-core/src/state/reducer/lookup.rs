//! Finding a step or attempt in a run, and building errors.

use crate::compiler::CompiledOrchestration;
use crate::definition::OrchestrationNodeId;

use crate::state::{
    OrchestrationRunState, OrchestrationStatus, StepAttempt, StepRun, TransitionError,
};

pub(super) fn step_mut<'a>(
    compiled: &CompiledOrchestration,
    state: &'a mut OrchestrationRunState,
    node_id: &OrchestrationNodeId,
) -> Result<&'a mut StepRun, TransitionError> {
    if compiled.node(node_id).is_none() {
        return Err(transition_error(
            "unknown_step",
            format!("unknown step {node_id}"),
        ));
    }
    state.steps.get_mut(node_id).ok_or_else(|| {
        transition_error("missing_step_state", format!("missing state for {node_id}"))
    })
}

/// The step, if `attempt` is its in-flight attempt. Rejects completions from
/// stale attempts so an old delegated run cannot complete a newer retry.
pub(super) fn active_attempt_mut<'a>(
    compiled: &CompiledOrchestration,
    state: &'a mut OrchestrationRunState,
    node_id: &OrchestrationNodeId,
    attempt: u32,
) -> Result<&'a mut StepRun, TransitionError> {
    let step = step_mut(compiled, state, node_id)?;
    if !step.status.is_active() {
        return Err(transition_error(
            "step_not_running",
            format!("step {node_id} is not running"),
        ));
    }
    if step.attempts.last().map(|value| value.attempt) != Some(attempt) {
        return Err(transition_error(
            "stale_attempt",
            format!("attempt {attempt} is not active"),
        ));
    }
    Ok(step)
}

pub(super) fn require_status(
    actual: OrchestrationStatus,
    expected: OrchestrationStatus,
) -> Result<(), TransitionError> {
    if actual == expected {
        Ok(())
    } else {
        Err(transition_error(
            "invalid_run_status",
            format!("expected run status {expected:?}, got {actual:?}"),
        ))
    }
}

pub(super) fn transition_error(code: &'static str, message: impl Into<String>) -> TransitionError {
    TransitionError {
        code,
        message: message.into(),
    }
}

/// The run state of a step. Every node of a compiled workflow has one, so a miss means the state
/// and the workflow disagree.
pub(super) fn step_state<'a>(
    state: &'a mut OrchestrationRunState,
    node_id: &OrchestrationNodeId,
) -> Result<&'a mut StepRun, TransitionError> {
    state.steps.get_mut(node_id).ok_or_else(|| {
        transition_error("missing_step_state", format!("missing state for {node_id}"))
    })
}

/// The step's in-flight attempt, which every active step has.
pub(super) fn current_attempt(step: &mut StepRun) -> Result<&mut StepAttempt, TransitionError> {
    step.attempts
        .last_mut()
        .ok_or_else(|| transition_error("no_active_attempt", "active step has no attempt"))
}
