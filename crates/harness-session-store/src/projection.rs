//! Side-effect-free projection of validated trailing events onto a snapshot.
//!
//! This reducer deliberately handles only information carried by durable
//! events. It never invents transcript text, tool inputs, backends, or child
//! state that the event schema does not contain.

use harness_protocol::commands::AgentStatus;
use harness_protocol::events::AgentEvent;

use crate::replay::ReplayError;
use crate::store::{DurableSessionEvent, DurableSessionSnapshot};

/// Applies already validated trailing events to a migrated checkpoint.
///
/// The returned snapshot is the exact input to runtime reconstruction. Unknown
/// agents and inconsistent state are rejected instead of being silently
/// dropped or replaced with fake state.
pub fn replay_snapshot(
    mut snapshot: DurableSessionSnapshot,
    events: &[DurableSessionEvent],
) -> Result<DurableSessionSnapshot, ReplayError> {
    for event in events {
        let sequence = event
            .session_sequence
            .ok_or_else(|| ReplayError::CorruptPayload {
                session_sequence: None,
                reason: "durable event carries no final session sequence".into(),
            })?;
        let agent_id = event.envelope.agent_id;
        let agent = snapshot
            .agents
            .iter_mut()
            .find(|agent| agent.agent_id == agent_id)
            .ok_or_else(|| ReplayError::InvalidTransition {
                session_sequence: sequence,
                reason: format!(
                    "event targets agent {agent_id}, which is absent from the checkpoint"
                ),
            })?;

        match &event.envelope.event {
            AgentEvent::StateChanged { from, to } => {
                if agent.status != *from {
                    return Err(ReplayError::InvalidTransition {
                        session_sequence: sequence,
                        reason: format!(
                            "agent {agent_id} is in {:?}, event claims {from:?}",
                            agent.status
                        ),
                    });
                }
                agent.status = *to;
                agent.transition_sequence = event.envelope.agent_sequence;
                if matches!(
                    to,
                    AgentStatus::Idle
                        | AgentStatus::Completed
                        | AgentStatus::Cancelled
                        | AgentStatus::Failed
                ) {
                    agent.current_operation = None;
                }
            }
            AgentEvent::RunStarted { run_id } => {
                agent.active_run = Some(*run_id);
                agent.last_error = None;
            }
            AgentEvent::PermissionRequested { request } => {
                agent
                    .pending_permissions
                    .insert(request.id, request.tool_call.id);
            }
            AgentEvent::ToolCallCompleted { call_id, .. } => {
                agent.pending_tools.remove(call_id);
                agent
                    .pending_permissions
                    .retain(|_, pending_call| *pending_call != *call_id);
            }
            AgentEvent::ChildAgentSpawned {
                agent_id: child_id, ..
            } => {
                if !agent.children.contains(child_id) {
                    agent.children.push(*child_id);
                }
            }
            AgentEvent::Failed { error } => {
                agent.last_error = Some(error.clone());
                agent.active_run = None;
            }
            AgentEvent::Completed { .. } => {
                agent.active_run = None;
            }
            AgentEvent::AssistantMessageCompleted { .. }
            | AgentEvent::ToolCallStarted { .. }
            | AgentEvent::ChildAgentCompleted { .. }
            | AgentEvent::UsageUpdated { .. }
            | AgentEvent::BackendRequestStarted { .. }
            | AgentEvent::AssistantMessageStarted { .. }
            | AgentEvent::AssistantTextDelta { .. }
            | AgentEvent::ReasoningDelta { .. }
            | AgentEvent::ToolCallRequested { .. }
            | AgentEvent::ToolCallProgress { .. }
            // Behavior state travels in snapshots, not in replayed events.
            | AgentEvent::BehaviorRuleFired { .. }
            | AgentEvent::ContextInjected { .. }
            | AgentEvent::ToolCallDenied { .. }
            | AgentEvent::CompletionGateEvaluated { .. }
            | AgentEvent::ModelRequestPrepared { .. }
            | AgentEvent::ProfileChanged { .. } => {}
        }

        snapshot.session_sequence = sequence;
        snapshot.timestamp = event.envelope.timestamp;
    }

    Ok(snapshot)
}

#[cfg(test)]
mod tests;
