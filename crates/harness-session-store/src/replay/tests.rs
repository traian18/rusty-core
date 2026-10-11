use super::*;
use crate::store::{DurableSessionMetadata, DurableSessionSnapshot, StoredAgentState};
use crate::version::SCHEMA_VERSION;
use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference,
};
use harness_protocol::commands::AgentOperation;
use harness_protocol::events::{AgentEventEnvelope, EventVisibility};
use harness_protocol::ids::{BackendId, ConfigurationId, EventId, IntegrationId, RunId, Timestamp};
use harness_protocol::usage::AgentBudget;

fn envelope(session: SessionId, agent: AgentId, seq: u64, event: AgentEvent) -> AgentEventEnvelope {
    AgentEventEnvelope {
        event_id: EventId::new(),
        session_id: session,
        agent_id: agent,
        parent_agent_id: None,
        run_id: Some(RunId::new()),
        agent_sequence: seq,
        session_sequence: Some(seq),
        timestamp: Timestamp::now(),
        visibility: EventVisibility::User,
        event,
    }
}

fn state_event(
    session: SessionId,
    agent: AgentId,
    seq: u64,
    from: AgentStatus,
    to: AgentStatus,
) -> DurableSessionEvent {
    DurableSessionEvent {
        session_sequence: Some(seq),
        envelope: envelope(session, agent, seq, AgentEvent::StateChanged { from, to }),
    }
}

fn stored(session: SessionId, events: Vec<DurableSessionEvent>) -> StoredSession {
    StoredSession {
        session_id: session,
        snapshot: None,
        events,
    }
}

fn snapshot(
    session: SessionId,
    agent: AgentId,
    seq: u64,
    status: AgentStatus,
) -> DurableSessionSnapshot {
    DurableSessionSnapshot {
        session_id: session,
        root_agent_id: agent,
        agents: vec![StoredAgentState {
            agent_id: agent,
            parent_id: None,
            status,
            current_operation: None::<AgentOperation>,
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
        }],
        session_sequence: seq,
        timestamp: Timestamp::now(),
        schema_version: SCHEMA_VERSION,
        metadata: DurableSessionMetadata::default(),
    }
}

#[test]
fn gapless_durable_stream_is_accepted() {
    let session = SessionId::new();
    let agent = AgentId::new();
    let events = vec![
        state_event(
            session,
            agent,
            1,
            AgentStatus::Idle,
            AgentStatus::PreparingContext,
        ),
        state_event(
            session,
            agent,
            2,
            AgentStatus::PreparingContext,
            AgentStatus::Streaming,
        ),
    ];
    assert_eq!(
        validate_trailing_replay(&stored(session, events), GapPolicy::Strict)
            .expect("valid stream")
            .len(),
        2
    );
}

#[test]
fn duplicate_sequence_is_rejected() {
    let session = SessionId::new();
    let agent = AgentId::new();
    let events = vec![
        state_event(
            session,
            agent,
            1,
            AgentStatus::Idle,
            AgentStatus::PreparingContext,
        ),
        state_event(
            session,
            agent,
            1,
            AgentStatus::PreparingContext,
            AgentStatus::Streaming,
        ),
    ];
    assert!(matches!(
        validate_trailing_replay(&stored(session, events), GapPolicy::Strict),
        Err(ReplayError::DuplicateSequence(1))
    ));
}

#[test]
fn out_of_order_sequences_are_rejected_without_sorting() {
    let session = SessionId::new();
    let agent = AgentId::new();
    let events = vec![
        state_event(
            session,
            agent,
            2,
            AgentStatus::Idle,
            AgentStatus::PreparingContext,
        ),
        state_event(
            session,
            agent,
            1,
            AgentStatus::PreparingContext,
            AgentStatus::Streaming,
        ),
    ];
    assert!(matches!(
        validate_trailing_replay(&stored(session, events), GapPolicy::Strict),
        Err(ReplayError::OutOfOrder {
            previous: 2,
            next: 1
        })
    ));
}

#[test]
fn snapshot_state_seeds_transition_validation() {
    let session = SessionId::new();
    let agent = AgentId::new();
    let mut stored = stored(
        session,
        vec![state_event(
            session,
            agent,
            4,
            AgentStatus::Streaming,
            AgentStatus::Idle,
        )],
    );
    stored.snapshot = Some(snapshot(session, agent, 3, AgentStatus::Idle));
    assert!(matches!(
        validate_trailing_replay(&stored, GapPolicy::Strict),
        Err(ReplayError::InvalidTransition { .. })
    ));
}

#[test]
fn concurrent_agents_have_independent_transition_state() {
    let session = SessionId::new();
    let first = AgentId::new();
    let second = AgentId::new();
    let events = vec![
        state_event(
            session,
            first,
            1,
            AgentStatus::Idle,
            AgentStatus::PreparingContext,
        ),
        state_event(
            session,
            second,
            2,
            AgentStatus::Idle,
            AgentStatus::PreparingContext,
        ),
        state_event(
            session,
            first,
            3,
            AgentStatus::PreparingContext,
            AgentStatus::Streaming,
        ),
    ];
    validate_trailing_replay(&stored(session, events), GapPolicy::Strict)
        .expect("interleaved agents validate independently");
}

#[test]
fn strict_gap_after_snapshot_is_rejected() {
    let session = SessionId::new();
    let agent = AgentId::new();
    let mut stored = stored(
        session,
        vec![state_event(
            session,
            agent,
            5,
            AgentStatus::Idle,
            AgentStatus::PreparingContext,
        )],
    );
    stored.snapshot = Some(snapshot(session, agent, 2, AgentStatus::Idle));
    assert!(matches!(
        validate_trailing_replay(&stored, GapPolicy::Strict),
        Err(ReplayError::Gap {
            expected: 3,
            found: 5
        })
    ));
}

#[test]
fn future_snapshot_version_is_rejected() {
    let session = SessionId::new();
    let agent = AgentId::new();
    let mut stored = stored(session, Vec::new());
    let mut checkpoint = snapshot(session, agent, 0, AgentStatus::Idle);
    checkpoint.schema_version = SCHEMA_VERSION + 1;
    stored.snapshot = Some(checkpoint);
    assert!(matches!(
        validate_trailing_replay(&stored, GapPolicy::Strict),
        Err(ReplayError::FutureSnapshotVersion { .. })
    ));
}
