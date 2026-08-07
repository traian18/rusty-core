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
            AgentEvent::ChildAgentSpawned { agent_id: child_id } => {
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
            | AgentEvent::ToolCallProgress { .. } => {}
        }

        snapshot.session_sequence = sequence;
        snapshot.timestamp = event.envelope.timestamp;
    }

    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::store::{
        DurableSessionMetadata, StoredAgentState,
    };
    use crate::version::SCHEMA_VERSION;
    use harness_protocol::backend::{
        BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference,
    };
    use harness_protocol::events::{AgentEventEnvelope, AgentOutcome, EventVisibility};
    use harness_protocol::ids::{
        AgentId, BackendId, ConfigurationId, EventId, IntegrationId, RunId, SessionId,
        Timestamp,
    };
    use harness_protocol::usage::AgentBudget;

    fn stored_agent(agent_id: AgentId) -> StoredAgentState {
        StoredAgentState {
            agent_id,
            parent_id: None,
            status: AgentStatus::Idle,
            current_operation: None,
            system_prompt: String::new(),
            execution_params: Default::default(),
            messages: Vec::new(),
            active_run: None,
            pending_tools: HashMap::new(),
            pending_permissions: HashMap::new(),
            children: Vec::new(),
            last_error: None,
            transition_sequence: 0,
            depth: 0,
            backend: BackendBinding {
                reference: BackendReference {
                    integration: IntegrationId::new(),
                    configuration: ConfigurationId::new(),
                    model: None,
                },
                descriptor: BackendDescriptor {
                    id: BackendId::new(),
                    name: "test".into(),
                    description: "test backend".into(),
                    capabilities: BackendCapabilities::default(),
                },
            },
            backend_config: serde_json::Value::Null,
            budget: AgentBudget::default(),
            capabilities: serde_json::Value::Null,
            usage: serde_json::Value::Null,
        }
    }

    fn event(
        session_id: SessionId,
        agent_id: AgentId,
        sequence: u64,
        run_id: RunId,
        payload: AgentEvent,
    ) -> DurableSessionEvent {
        DurableSessionEvent {
            session_sequence: Some(sequence),
            envelope: AgentEventEnvelope {
                event_id: EventId::new(),
                session_id,
                agent_id,
                parent_agent_id: None,
                run_id: Some(run_id),
                agent_sequence: sequence,
                session_sequence: Some(sequence),
                timestamp: Timestamp::now(),
                visibility: EventVisibility::User,
                event: payload,
            },
        }
    }

    #[test]
    fn trailing_run_state_is_applied_to_checkpoint() {
        let session_id = SessionId::new();
        let agent_id = AgentId::new();
        let run_id = RunId::new();
        let snapshot = DurableSessionSnapshot {
            session_id,
            root_agent_id: agent_id,
            agents: vec![stored_agent(agent_id)],
            session_sequence: 4,
            timestamp: Timestamp::now(),
            schema_version: SCHEMA_VERSION,
            metadata: DurableSessionMetadata::default(),
        };
        let events = vec![
            event(
                session_id,
                agent_id,
                5,
                run_id,
                AgentEvent::RunStarted { run_id },
            ),
            event(
                session_id,
                agent_id,
                6,
                run_id,
                AgentEvent::StateChanged {
                    from: AgentStatus::Idle,
                    to: AgentStatus::PreparingContext,
                },
            ),
            event(
                session_id,
                agent_id,
                7,
                run_id,
                AgentEvent::Completed {
                    outcome: AgentOutcome::Cancelled,
                },
            ),
        ];

        let restored = replay_snapshot(snapshot, &events).expect("replay snapshot");
        let agent = &restored.agents[0];
        assert_eq!(restored.session_sequence, 7);
        assert_eq!(agent.status, AgentStatus::PreparingContext);
        assert_eq!(agent.active_run, None);
        assert_eq!(agent.transition_sequence, 6);
    }

    #[test]
    fn unknown_agent_is_rejected() {
        let session_id = SessionId::new();
        let root_id = AgentId::new();
        let unknown_id = AgentId::new();
        let run_id = RunId::new();
        let snapshot = DurableSessionSnapshot {
            session_id,
            root_agent_id: root_id,
            agents: vec![stored_agent(root_id)],
            session_sequence: 0,
            timestamp: Timestamp::now(),
            schema_version: SCHEMA_VERSION,
            metadata: DurableSessionMetadata::default(),
        };
        let events = vec![event(
            session_id,
            unknown_id,
            1,
            run_id,
            AgentEvent::RunStarted { run_id },
        )];

        assert!(matches!(
            replay_snapshot(snapshot, &events),
            Err(ReplayError::InvalidTransition { .. })
        ));
    }
}
