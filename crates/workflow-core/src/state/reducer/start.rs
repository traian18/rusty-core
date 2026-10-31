//! Starting a run and admitting each step.

use serde_json::Value;

use crate::compiler::CompiledOrchestration;
use crate::definition::OrchestrationNodeId;

use super::control::settle;
use super::finish::terminate_failed;
use super::lookup::require_status;
use super::lookup::step_mut;
use super::lookup::step_state;
use super::lookup::transition_error;
use crate::state::{
    error_codes, AttemptDetails, AttemptStatus, OrchestrationEffect, OrchestrationError,
    OrchestrationEvent, OrchestrationRunState, OrchestrationStatus, StepAttempt, StepStatus,
    TransitionError, UsageSummary,
};

pub(super) fn start(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    input: Value,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    require_status(state.status, OrchestrationStatus::Created)?;
    state.input = Some(input);
    let step = step_state(state, &compiled.entry_node)?;
    step.status = StepStatus::Ready;
    settle(state);
    Ok(vec![
        OrchestrationEffect::Emit(OrchestrationEvent::RunStarted),
        OrchestrationEffect::Emit(OrchestrationEvent::StepReady {
            node_id: compiled.entry_node.clone(),
        }),
    ])
}

/// Returns the first budget dimension that is exhausted, if any.
pub(super) fn exhausted_budget(
    compiled: &CompiledOrchestration,
    state: &OrchestrationRunState,
) -> Option<String> {
    let policies = &compiled.definition.policies;
    let usage = &state.usage;
    if let Some(limit) = policies
        .max_total_attempts
        .filter(|&limit| state.total_attempts >= limit)
    {
        return Some(format!("attempt budget of {limit} exhausted"));
    }
    let checks = [
        (
            "model request",
            policies.max_model_requests,
            usage.model_requests,
        ),
        ("tool call", policies.max_tool_calls, usage.tool_calls),
        ("token", policies.max_tokens, usage.tokens),
    ];
    for (name, limit, used) in checks {
        if let Some(limit) = limit {
            if used >= limit {
                return Some(format!("{name} budget of {limit} exhausted ({used} used)"));
            }
        }
    }
    if let Some(limit) = policies.max_cost_usd {
        if usage.cost_usd >= limit {
            return Some(format!(
                "cost budget of ${limit} exhausted (${} used)",
                usage.cost_usd
            ));
        }
    }
    None
}

pub(super) fn admit(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
    attempt: u32,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    if state.status != OrchestrationStatus::Ready || state.paused {
        return Err(transition_error(
            "run_not_ready",
            format!("run in status {:?} cannot admit work", state.status),
        ));
    }
    {
        let step = step_mut(compiled, state, &node_id)?;
        if step.status != StepStatus::Ready {
            return Err(transition_error(
                "step_not_ready",
                format!("step {node_id} is not ready"),
            ));
        }
        let expected_attempt = step.attempts.len() as u32 + 1;
        if attempt != expected_attempt {
            return Err(transition_error(
                "invalid_attempt",
                format!("expected attempt {expected_attempt}, got {attempt}"),
            ));
        }
    }
    if let Some(reason) = exhausted_budget(compiled, state) {
        return terminate_failed(
            state,
            OrchestrationError::new(error_codes::BUDGET_EXHAUSTED, reason),
            Vec::new(),
        );
    }
    let step = step_state(state, &node_id)?;
    step.status = StepStatus::Running;
    step.attempts.push(StepAttempt {
        attempt,
        status: AttemptStatus::Running,
        output: None,
        error: None,
        details: AttemptDetails::default(),
    });
    state.total_attempts += 1;
    settle(state);
    Ok(vec![
        OrchestrationEffect::Emit(OrchestrationEvent::StepStarted {
            node_id: node_id.clone(),
            attempt,
        }),
        OrchestrationEffect::ExecuteStep { node_id, attempt },
    ])
}

pub(super) fn record_usage(
    state: &mut OrchestrationRunState,
    details: &AttemptDetails,
) -> Option<OrchestrationEffect> {
    let usage = &details.usage;
    if *usage == UsageSummary::default() {
        return None;
    }
    state.usage.add(usage);
    Some(OrchestrationEffect::Emit(
        OrchestrationEvent::BudgetUpdated {
            usage: state.usage.clone(),
        },
    ))
}
