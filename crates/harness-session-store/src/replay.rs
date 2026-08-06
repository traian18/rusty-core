//! Side-effect-free trailing replay validation (RC-303).
//!
//! Restore consumes records in the order returned by the store. Validation
//! never sorts corrupt input into a valid-looking stream and never performs
//! provider, tool, permission, or external-sink I/O.

use std::collections::{HashMap, HashSet};

use harness_protocol::commands::AgentStatus;
use harness_protocol::events::AgentEvent;
use harness_protocol::ids::{AgentId, SessionId};

use crate::store::{DurableSessionEvent, StoredSession};
use crate::version::{check_snapshot_version, SnapshotVersionError};

/// Typed errors surfaced by trailing replay validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplayError {
    #[error("session not found: {0}")]
    NotFound(SessionId),
    #[error("snapshot schema version {found} is newer than supported ({supported})")]
    FutureSnapshotVersion { found: u64, supported: u64 },
    #[error("snapshot schema version {found} is older than the oldest supported version ({supported})")]
    AncientSnapshotVersion { found: u64, supported: u64 },
    #[error("duplicate event id {event_id} at session sequence {session_sequence}")]
    DuplicateEventId {
        event_id: String,
        session_sequence: u64,
    },
    #[error("duplicate session sequence {0}")]
    DuplicateSequence(u64),
    #[error("out-of-order durable sequences: {previous} followed by {next}")]
    OutOfOrder { previous: u64, next: u64 },
    #[error("durable sequence gap after snapshot: expected {expected}, found {found}")]
    Gap { expected: u64, found: u64 },
    #[error("corrupt durable payload at session sequence {session_sequence:?}: {reason}")]
    CorruptPayload {
        session_sequence: Option<u64>,
        reason: String,
    },
    #[error("invalid transition at session sequence {session_sequence}: {reason}")]
    InvalidTransition {
        session_sequence: u64,
        reason: String,
    },
}

/// How strictly durable-sequence gaps are interpreted during replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GapPolicy {
    Strict,
    #[default]
    AllowEphemeralHoles,
}

/// A validating replay driver.
#[derive(Debug, Clone, Copy)]
pub struct ReplayValidator {
    gap_policy: GapPolicy,
}

impl ReplayValidator {
    pub fn new(gap_policy: GapPolicy) -> Self {
        Self { gap_policy }
    }

    pub fn validate(
        &self,
        stored: &StoredSession,
    ) -> Result<Vec<DurableSessionEvent>, ReplayError> {
        validate_trailing_replay(stored, self.gap_policy)
    }
}

/// Validates the stored trailing stream without reordering or side effects.
pub fn validate_trailing_replay(
    stored: &StoredSession,
    gap_policy: GapPolicy,
) -> Result<Vec<DurableSessionEvent>, ReplayError> {
    if let Some(snapshot) = &stored.snapshot {
        check_snapshot_version(snapshot.schema_version).map_err(|error| match error {
            SnapshotVersionError::FutureVersion { found, supported } => {
                ReplayError::FutureSnapshotVersion { found, supported }
            }
            SnapshotVersionError::AncientVersion { found, supported } => {
                ReplayError::AncientSnapshotVersion { found, supported }
            }
        })?;
    }

    let mut seen_event_ids = HashSet::new();
    let mut previous = stored
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.session_sequence);
    let mut validated = Vec::with_capacity(stored.events.len());

    for event in &stored.events {
        let sequence = event.session_sequence.ok_or_else(|| ReplayError::CorruptPayload {
            session_sequence: None,
            reason: "durable event carries no final session sequence".into(),
        })?;

        if event.envelope.session_sequence != Some(sequence) {
            return Err(ReplayError::CorruptPayload {
                session_sequence: Some(sequence),
                reason: "durable record and envelope session sequences disagree".into(),
            });
        }
        if event.envelope.session_id != stored.session_id {
            return Err(ReplayError::CorruptPayload {
                session_sequence: Some(sequence),
                reason: format!(
                    "event belongs to session {}, expected {}",
                    event.envelope.session_id, stored.session_id
                ),
            });
        }

        let event_id = event.envelope.event_id.to_string();
        if !seen_event_ids.insert(event_id.clone()) {
            return Err(ReplayError::DuplicateEventId {
                event_id,
                session_sequence: sequence,
            });
        }

        match previous {
            Some(value) if sequence == value => {
                return Err(ReplayError::DuplicateSequence(sequence));
            }
            Some(value) if sequence < value => {
                return Err(ReplayError::OutOfOrder {
                    previous: value,
                    next: sequence,
                });
            }
            Some(value)
                if gap_policy == GapPolicy::Strict
                    && sequence > value.saturating_add(1) =>
            {
                return Err(ReplayError::Gap {
                    expected: value.saturating_add(1),
                    found: sequence,
                });
            }
            _ => {}
        }
        previous = Some(sequence);

        validate_payload(event, sequence)?;
        validated.push(event.clone());
    }

    validate_transition_stream(stored, &validated)?;
    Ok(validated)
}

fn validate_payload(event: &DurableSessionEvent, sequence: u64) -> Result<(), ReplayError> {
    let payload = serde_json::to_value(&event.envelope).map_err(|error| {
        ReplayError::CorruptPayload {
            session_sequence: Some(sequence),
            reason: error.to_string(),
        }
    })?;
    serde_json::to_string(&payload).map_err(|error| ReplayError::CorruptPayload {
        session_sequence: Some(sequence),
        reason: error.to_string(),
    })?;
    Ok(())
}

/// Validates transitions independently for each agent, seeded from the
/// checkpoint when one exists.
fn validate_transition_stream(
    stored: &StoredSession,
    events: &[DurableSessionEvent],
) -> Result<(), ReplayError> {
    let mut statuses: HashMap<AgentId, AgentStatus> = stored
        .snapshot
        .iter()
        .flat_map(|snapshot| snapshot.agents.iter())
        .map(|agent| (agent.agent_id, agent.status))
        .collect();

    for event in events {
        let sequence = event
            .session_sequence
            .expect("validated events always carry a sequence");
        if let AgentEvent::StateChanged { from, to } = &event.envelope.event {
            if let Some(current) = statuses.get(&event.envelope.agent_id) {
                if current != from {
                    return Err(ReplayError::InvalidTransition {
                        session_sequence: sequence,
                        reason: format!(
                            "agent {} is in {current:?}, event claims {from:?}",
                            event.envelope.agent_id
                        ),
                    });
                }
                if is_terminal(*current) {
                    return Err(ReplayError::InvalidTransition {
                        session_sequence: sequence,
                        reason: format!(
                            "terminal state {current:?} is absorbing; cannot transition to {to:?}"
                        ),
                    });
                }
            }
            statuses.insert(event.envelope.agent_id, *to);
        }
    }
    Ok(())
}

fn is_terminal(status: AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::Cancelled | AgentStatus::Completed | AgentStatus::Failed
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        DurableSessionMetadata, DurableSessionSnapshot, StoredAgentState,
    };
    use crate::version::SCHEMA_VERSION;
    use harness_protocol::backend::{
        BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference,
    };
    use harness_protocol::commands::AgentOperation;
    use harness_protocol::events::{AgentEventEnvelope, EventVisibility};
    use harness_protocol::ids::{
        BackendId, ConfigurationId, EventId, IntegrationId, RunId, Timestamp,
    };
    use harness_protocol::usage::AgentBudget;

    fn envelope(
        session: SessionId,
        agent: AgentId,
        seq: u64,
        event: AgentEvent,
    ) -> AgentEventEnvelope {
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
            envelope: envelope(
                session,
                agent,
                seq,
                AgentEvent::StateChanged { from, to },
            ),
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
            state_event(session, agent, 1, AgentStatus::Idle, AgentStatus::PreparingContext),
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
            state_event(session, agent, 1, AgentStatus::Idle, AgentStatus::PreparingContext),
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
            state_event(session, agent, 2, AgentStatus::Idle, AgentStatus::PreparingContext),
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
            state_event(session, first, 1, AgentStatus::Idle, AgentStatus::PreparingContext),
            state_event(session, second, 2, AgentStatus::Idle, AgentStatus::PreparingContext),
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
}
