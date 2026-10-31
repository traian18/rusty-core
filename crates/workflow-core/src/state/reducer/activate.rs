//! Making a step active, and trying it again.

use crate::compiler::CompiledOrchestration;
use crate::definition::{EdgeCondition, OrchestrationNodeId};

use super::control::settle;
use super::lookup::step_mut;
use super::lookup::step_state;
use super::lookup::transition_error;
use crate::state::{
    OrchestrationEffect, OrchestrationEvent, OrchestrationRunState, StepStatus, TransitionError,
};

pub(super) fn activate(
    state: &mut OrchestrationRunState,
    from: &OrchestrationNodeId,
    target: &OrchestrationNodeId,
    condition: EdgeCondition,
    effects: &mut Vec<OrchestrationEffect>,
) -> Result<(), TransitionError> {
    let next = step_state(state, target)?;
    if next.status != StepStatus::Pending {
        return Err(transition_error(
            "target_not_pending",
            format!("target step {target} is not pending"),
        ));
    }
    next.status = StepStatus::Ready;
    effects.push(OrchestrationEffect::Emit(
        OrchestrationEvent::TransitionSelected {
            from: from.clone(),
            to: target.clone(),
            condition,
        },
    ));
    effects.push(OrchestrationEffect::Emit(OrchestrationEvent::StepReady {
        node_id: target.clone(),
    }));
    Ok(())
}

pub(super) fn retry(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let step = step_mut(compiled, state, &node_id)?;
    if step.status != StepStatus::RetryScheduled {
        return Err(transition_error(
            "retry_not_scheduled",
            "step has no scheduled retry",
        ));
    }
    step.status = StepStatus::Ready;
    settle(state);
    Ok(vec![OrchestrationEffect::Emit(
        OrchestrationEvent::StepReady { node_id },
    )])
}
