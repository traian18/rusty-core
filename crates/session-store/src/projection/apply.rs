//! What each kind of event does to one agent's stored state.

use rusty_protocol::{
    commands::AgentStatus,
    events::{AgentEvent, AgentEventEnvelope},
};

use crate::{replay::ReplayError, store::StoredAgentState};

/// Applies `envelope.event` to `agent`. Events that change nothing a snapshot stores (streaming
/// deltas, usage, behavior state, which travels in snapshots) are ignored. They are listed one by
/// one so that a new event kind cannot be forgotten: the compiler asks for a decision.
pub(super) fn apply_event(
    agent: &mut StoredAgentState,
    envelope: &AgentEventEnvelope,
    sequence: u64,
) -> Result<(), ReplayError> {
    match &envelope.event {
        AgentEvent::StateChanged { from, to } => {
            change_state(agent, *from, *to, envelope.agent_sequence, sequence)?;
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
        AgentEvent::Completed { .. } => agent.active_run = None,
        AgentEvent::AssistantMessageCompleted { .. }
        | AgentEvent::ToolCallStarted { .. }
        | AgentEvent::ChildAgentCompleted { .. }
        | AgentEvent::UsageUpdated { .. }
        | AgentEvent::PromptPrefixChanged { .. }
        | AgentEvent::BackendRequestStarted { .. }
        | AgentEvent::ContextRequestStarted { .. }
        | AgentEvent::ContextUpdated { .. }
        | AgentEvent::CompactionStarted { .. }
        | AgentEvent::CompactionFinished { .. }
        | AgentEvent::AssistantMessageStarted { .. }
        | AgentEvent::AssistantTextDelta { .. }
        | AgentEvent::ReasoningDelta { .. }
        | AgentEvent::ToolCallRequested { .. }
        | AgentEvent::ToolCallProgress { .. }
        | AgentEvent::BehaviorRuleFired { .. }
        | AgentEvent::ContextInjected { .. }
        | AgentEvent::ToolCallDenied { .. }
        | AgentEvent::CompletionGateEvaluated { .. }
        | AgentEvent::ModelRequestPrepared { .. }
        | AgentEvent::ProfileChanged { .. } => {}
    }
    Ok(())
}

/// A transition must start from the state the agent is in; anything else means the log and the
/// checkpoint disagree.
fn change_state(
    agent: &mut StoredAgentState,
    from: AgentStatus,
    to: AgentStatus,
    agent_sequence: u64,
    sequence: u64,
) -> Result<(), ReplayError> {
    if agent.status != from {
        return Err(ReplayError::InvalidTransition {
            session_sequence: sequence,
            reason: format!(
                "agent {} is in {:?}, event claims {from:?}",
                agent.agent_id, agent.status
            ),
        });
    }
    agent.status = to;
    agent.transition_sequence = agent_sequence;
    if is_resting(to) {
        agent.current_operation = None;
    }
    Ok(())
}

fn is_resting(status: AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::Idle | AgentStatus::Completed | AgentStatus::Cancelled | AgentStatus::Failed
    )
}
