//! A step waiting for an answer from a person.

use crate::compiler::CompiledOrchestration;
use crate::definition::OrchestrationNodeId;

use super::control::settle;
use super::lookup::active_attempt_mut;
use super::lookup::transition_error;
use crate::state::{
    InputRequest, InputResponse, OrchestrationEffect, OrchestrationEvent, OrchestrationRunState,
    StepStatus, TransitionError,
};

pub(super) fn input_required(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
    attempt: u32,
    request: InputRequest,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let step = active_attempt_mut(compiled, state, &node_id, attempt)?;
    if step.pending_input.is_some() {
        return Err(transition_error(
            "input_pending",
            format!("step {node_id} is already waiting for an answer"),
        ));
    }
    if request.decisions.is_empty() {
        return Err(transition_error(
            "invalid_input_request",
            "a question needs at least one possible answer",
        ));
    }
    step.status = StepStatus::WaitingForInput;
    step.pending_input = Some(request.clone());
    settle(state);
    Ok(vec![OrchestrationEffect::Emit(
        OrchestrationEvent::InputRequested {
            node_id,
            attempt,
            request: Box::new(request),
        },
    )])
}

pub(super) fn input_resolved(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
    attempt: u32,
    request_id: String,
    response: InputResponse,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let step = active_attempt_mut(compiled, state, &node_id, attempt)?;
    let Some(request) = step
        .pending_input
        .as_ref()
        .filter(|request| request.id == request_id)
    else {
        return Err(transition_error(
            "unknown_input",
            format!("step {node_id} is not waiting on question {request_id}"),
        ));
    };
    let Some(decision) = request
        .decisions
        .iter()
        .find(|decision| decision.id == response.decision)
    else {
        return Err(transition_error(
            "invalid_decision",
            format!(
                "{} is not one of the answers to question {request_id}",
                response.decision
            ),
        ));
    };
    if decision.requires_text
        && !matches!(response.text.as_deref(), Some(text) if !text.trim().is_empty())
    {
        return Err(transition_error(
            "missing_text",
            format!("the answer {} needs text", decision.id),
        ));
    }
    step.pending_input = None;
    step.status = if step.pending_permissions.is_empty() {
        StepStatus::Running
    } else {
        StepStatus::WaitingForPermission
    };
    settle(state);
    Ok(vec![OrchestrationEffect::Emit(
        OrchestrationEvent::InputResolved {
            node_id,
            attempt,
            request_id,
            response: Box::new(response),
        },
    )])
}
