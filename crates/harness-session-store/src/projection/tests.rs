use std::collections::HashMap;

use super::*;
use crate::store::{DurableSessionMetadata, StoredAgentState};
use crate::version::SCHEMA_VERSION;
use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference,
};
use harness_protocol::events::{AgentEventEnvelope, AgentOutcome, EventVisibility};
use harness_protocol::ids::{
    AgentId, BackendId, ConfigurationId, EventId, IntegrationId, RunId, SessionId, Timestamp,
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
        behavior: None,
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
