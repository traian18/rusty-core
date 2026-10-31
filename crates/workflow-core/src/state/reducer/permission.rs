//! A step waiting for permission to use a tool.

use crate::compiler::CompiledOrchestration;
use crate::definition::OrchestrationNodeId;

use super::control::settle;
use super::lookup::active_attempt_mut;
use super::lookup::transition_error;
use crate::state::{
    OrchestrationEffect, OrchestrationEvent, OrchestrationRunState, StepStatus, TransitionError,
};

pub(super) fn permission_required(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
    attempt: u32,
    permission_id: String,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let step = active_attempt_mut(compiled, state, &node_id, attempt)?;
    if step.pending_permissions.contains(&permission_id) {
        return Err(transition_error(
            "duplicate_permission",
            format!("permission {permission_id} is already pending"),
        ));
    }
    step.status = StepStatus::WaitingForPermission;
    step.pending_permissions.push(permission_id.clone());
    settle(state);
    Ok(vec![OrchestrationEffect::Emit(
        OrchestrationEvent::PermissionRequested {
            node_id,
            attempt,
            permission_id,
        },
    )])
}

pub(super) fn permission_resolved(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
    attempt: u32,
    permission_id: String,
    approved: bool,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let step = active_attempt_mut(compiled, state, &node_id, attempt)?;
    let Some(index) = step
        .pending_permissions
        .iter()
        .position(|pending| *pending == permission_id)
    else {
        return Err(transition_error(
            "unknown_permission",
            format!("step {node_id} is not waiting on permission {permission_id}"),
        ));
    };
    // A denial is returned to the agent, as in a direct session; the step
    // only fails if the agent cannot continue without the tool.
    step.pending_permissions.remove(index);
    if step.pending_permissions.is_empty() {
        step.status = StepStatus::Running;
    }
    settle(state);
    Ok(vec![OrchestrationEffect::Emit(
        OrchestrationEvent::PermissionResolved {
            node_id,
            attempt,
            permission_id,
            approved,
        },
    )])
}
