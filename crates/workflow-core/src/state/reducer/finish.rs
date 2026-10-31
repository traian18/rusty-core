//! How a step ends: success, failure, or sent back for another try.

use serde_json::Value;

use crate::compiler::CompiledOrchestration;
use crate::definition::{
    EdgeCondition, OrchestrationNode, OrchestrationNodeId, OrchestrationNodeKind, RetryReason,
};

use super::activate::activate;
use super::control::settle;
use super::lookup::{active_attempt_mut, current_attempt, step_state, transition_error};
use super::start::exhausted_budget;
use super::start::record_usage;
use crate::state::{
    error_codes, AttemptDetails, AttemptStatus, OrchestrationEffect, OrchestrationError,
    OrchestrationEvent, OrchestrationOutcome, OrchestrationResult, OrchestrationRunState,
    OrchestrationStatus, StepStatus, TransitionError,
};

pub(super) fn succeed(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
    attempt: u32,
    output: Value,
    details: AttemptDetails,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let is_output = matches!(
        compiled.node(&node_id).map(|node| &node.kind),
        Some(OrchestrationNodeKind::Output(_))
    );
    let usage_effect = record_usage(state, &details);
    {
        let step = active_attempt_mut(compiled, state, &node_id, attempt)?;
        if step.status != StepStatus::Running {
            return Err(transition_error(
                "step_not_running",
                format!("step {node_id} cannot succeed while {:?}", step.status),
            ));
        }
        let current = current_attempt(step)?;
        current.status = AttemptStatus::Succeeded;
        current.output = Some(output.clone());
        current.details = details;
        step.status = StepStatus::Succeeded;
        step.output = Some(output.clone());
        step.error = None;
    }
    let mut effects = Vec::from_iter(usage_effect);
    effects.push(OrchestrationEffect::Emit(
        OrchestrationEvent::StepSucceeded {
            node_id: node_id.clone(),
            attempt,
        },
    ));
    if is_output {
        state.final_output = Some(output);
        state.status = OrchestrationStatus::Completed;
        effects.push(OrchestrationEffect::Emit(OrchestrationEvent::RunCompleted));
        effects.push(OrchestrationEffect::Finish(
            OrchestrationResult::from_state(state, OrchestrationOutcome::Completed),
        ));
        return Ok(effects);
    }
    match compiled.outgoing_for(&node_id, EdgeCondition::OnSuccess) {
        Some(edge) => {
            let target = edge.target.clone();
            activate(
                state,
                &node_id,
                &target,
                EdgeCondition::OnSuccess,
                &mut effects,
            )?;
            settle(state);
            Ok(effects)
        }
        None => terminate_failed(
            state,
            OrchestrationError::new(
                error_codes::MISSING_TRANSITION,
                format!("step {node_id} has no success route"),
            ),
            effects,
        ),
    }
}

/// An attempt that has just failed, with what every recovery needs to know.
struct FailedAttempt<'a> {
    compiled: &'a CompiledOrchestration,
    node: &'a OrchestrationNode,
    node_id: &'a OrchestrationNodeId,
    attempt: u32,
    error: &'a OrchestrationError,
    /// Whether the run may still start new attempts.
    budget_left: bool,
}

/// Records a failed attempt, then tries the ways out in order: retry the step, send work back to
/// a retry target, ask for changes, take the failure route. With none of those the run fails.
pub(super) fn fail(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
    attempt: u32,
    error: OrchestrationError,
    details: AttemptDetails,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let node = compiled
        .node(&node_id)
        .ok_or_else(|| transition_error("unknown_step", format!("unknown step {node_id}")))?;
    let usage_effect = record_usage(state, &details);
    mark_attempt_failed(compiled, state, &node_id, attempt, &error, details)?;
    let mut effects = Vec::from_iter(usage_effect);
    effects.push(OrchestrationEffect::Emit(OrchestrationEvent::StepFailed {
        node_id: node_id.clone(),
        attempt,
        error: error.clone(),
    }));

    let failed = FailedAttempt {
        compiled,
        node,
        node_id: &node_id,
        attempt,
        error: &error,
        budget_left: exhausted_budget(compiled, state).is_none(),
    };
    if retry_same_step(state, &failed, &mut effects)?
        || send_back_to_retry_target(state, &failed, &mut effects)?
        || send_back_for_changes(state, &failed, &mut effects)?
        || take_failure_route(state, &failed, &mut effects)?
    {
        return Ok(effects);
    }
    let run_error = run_error(&failed);
    state.failed_step = Some(node_id.clone());
    terminate_failed(state, run_error, effects)
}

fn mark_attempt_failed(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: &OrchestrationNodeId,
    attempt: u32,
    error: &OrchestrationError,
    details: AttemptDetails,
) -> Result<(), TransitionError> {
    let step = active_attempt_mut(compiled, state, node_id, attempt)?;
    let current = current_attempt(step)?;
    current.status = AttemptStatus::Failed;
    current.error = Some(error.clone());
    current.details = details;
    step.status = StepStatus::Failed;
    step.error = Some(error.clone());
    step.pending_permissions.clear();
    step.pending_input = None;
    Ok(())
}

/// The step's own retry policy: a retryable error, attempts left, and budget.
fn retry_same_step(
    state: &mut OrchestrationRunState,
    failed: &FailedAttempt<'_>,
    effects: &mut Vec<OrchestrationEffect>,
) -> Result<bool, TransitionError> {
    let retry = &failed.node.retry;
    let retryable = failed
        .error
        .retry_reason
        .is_some_and(|reason| retry.retry_on.contains(&reason));
    let step = step_state(state, failed.node_id)?;
    let attempts_left = failed.attempt.saturating_sub(step.retry_base) < retry.max_attempts;
    if !(retryable && attempts_left && failed.budget_left) {
        return Ok(false);
    }
    step.status = StepStatus::RetryScheduled;
    step.feedback.push(failed.error.clone());
    effects.push(OrchestrationEffect::Emit(
        OrchestrationEvent::StepRetryScheduled {
            node_id: failed.node_id.clone(),
            next_attempt: failed.attempt + 1,
            triggered_by: failed.node_id.clone(),
        },
    ));
    settle(state);
    Ok(true)
}

/// A verifier that found the work wanting sends it back to its upstream retry target.
fn send_back_to_retry_target(
    state: &mut OrchestrationRunState,
    failed: &FailedAttempt<'_>,
    effects: &mut Vec<OrchestrationEffect>,
) -> Result<bool, TransitionError> {
    let (OrchestrationNodeKind::Verify(config), Some(RetryReason::VerificationFailed)) =
        (&failed.node.kind, failed.error.retry_reason)
    else {
        return Ok(false);
    };
    let Some(target_id) = &config.retry_target else {
        return Ok(false);
    };
    let target = failed.compiled.node(target_id).ok_or_else(|| {
        transition_error("unknown_step", format!("unknown retry target {target_id}"))
    })?;
    let target_step = step_state(state, target_id)?;
    let target_attempts = target_step.attempts.len() as u32;
    let attempts_left =
        target_attempts.saturating_sub(target_step.retry_base) < target.retry.max_attempts;
    if !(attempts_left && failed.budget_left) {
        return Ok(false);
    }
    send_back(
        failed.compiled,
        state,
        failed.node_id,
        target_id,
        failed.error.clone(),
        effects,
    )?;
    Ok(true)
}

/// The user asked for changes: an approval sends the work back to its revise target (at most
/// `max_revisions` times), and a task queue sends a plan revision back to the step that wrote the
/// plan. Asking for changes never uses up the target's failure retries.
fn send_back_for_changes(
    state: &mut OrchestrationRunState,
    failed: &FailedAttempt<'_>,
    effects: &mut Vec<OrchestrationEffect>,
) -> Result<bool, TransitionError> {
    let revise_target = match (&failed.node.kind, failed.error.retry_reason) {
        (OrchestrationNodeKind::Approval(config), Some(RetryReason::ChangesRequested))
            if failed.attempt <= config.max_revisions =>
        {
            config.revise_target()
        }
        (OrchestrationNodeKind::Agent(_), Some(RetryReason::ChangesRequested)) => {
            failed.node.task_plan_source()
        }
        _ => None,
    };
    let Some(target_id) = revise_target else {
        return Ok(false);
    };
    if !(failed.budget_left && failed.compiled.retry_spans.contains_key(failed.node_id)) {
        return Ok(false);
    }
    let target_step = step_state(state, target_id)?;
    target_step.retry_base = target_step.attempts.len() as u32;
    send_back(
        failed.compiled,
        state,
        failed.node_id,
        target_id,
        failed.error.clone(),
        effects,
    )?;
    Ok(true)
}

/// An explicit `on_failure` edge takes the run elsewhere instead of ending it.
fn take_failure_route(
    state: &mut OrchestrationRunState,
    failed: &FailedAttempt<'_>,
    effects: &mut Vec<OrchestrationEffect>,
) -> Result<bool, TransitionError> {
    let Some(edge) = failed
        .compiled
        .outgoing_for(failed.node_id, EdgeCondition::OnFailure)
    else {
        return Ok(false);
    };
    let target = edge.target.clone();
    activate(
        state,
        failed.node_id,
        &target,
        EdgeCondition::OnFailure,
        effects,
    )?;
    settle(state);
    Ok(true)
}

/// The error the run ends with: the step's own, unless the budget is why it was not retried.
fn run_error(failed: &FailedAttempt<'_>) -> OrchestrationError {
    if failed.budget_left {
        failed.error.clone()
    } else {
        OrchestrationError::new(
            error_codes::BUDGET_EXHAUSTED,
            format!("retry denied by budget after: {}", failed.error.message),
        )
    }
}

/// Re-run `target_id` with `error` as feedback, resetting every step between it and `from` (the
/// verifier or approval that sent the work back).
pub(super) fn send_back(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    from: &OrchestrationNodeId,
    target_id: &OrchestrationNodeId,
    error: OrchestrationError,
    effects: &mut Vec<OrchestrationEffect>,
) -> Result<(), TransitionError> {
    for span_node in &compiled.retry_spans[from] {
        let step = step_state(state, span_node)?;
        step.status = StepStatus::Pending;
        step.output = None;
        if span_node != target_id {
            step.checkpoint = None;
        }
    }
    let target_step = step_state(state, target_id)?;
    let next_attempt = target_step.attempts.len() as u32 + 1;
    target_step.status = StepStatus::RetryScheduled;
    target_step.feedback.push(error);
    effects.push(OrchestrationEffect::Emit(
        OrchestrationEvent::StepRetryScheduled {
            node_id: target_id.clone(),
            next_attempt,
            triggered_by: from.clone(),
        },
    ));
    settle(state);
    Ok(())
}

pub(super) fn terminate_failed(
    state: &mut OrchestrationRunState,
    error: OrchestrationError,
    mut effects: Vec<OrchestrationEffect>,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    state.status = OrchestrationStatus::Failed;
    state.error = Some(error.clone());
    for step in state.steps.values_mut() {
        match step.status {
            StepStatus::Pending => step.status = StepStatus::Skipped,
            StepStatus::Ready | StepStatus::RetryScheduled => step.status = StepStatus::Cancelled,
            _ => {}
        }
    }
    effects.push(OrchestrationEffect::Emit(OrchestrationEvent::RunFailed {
        error,
    }));
    effects.push(OrchestrationEffect::Finish(
        OrchestrationResult::from_state(state, OrchestrationOutcome::Failed),
    ));
    Ok(effects)
}
