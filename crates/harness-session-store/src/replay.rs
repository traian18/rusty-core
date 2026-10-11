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
    #[error(
        "snapshot schema version {found} is older than the oldest supported version ({supported})"
    )]
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
        let sequence = event
            .session_sequence
            .ok_or_else(|| ReplayError::CorruptPayload {
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
                if gap_policy == GapPolicy::Strict && sequence > value.saturating_add(1) =>
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
    let payload =
        serde_json::to_value(&event.envelope).map_err(|error| ReplayError::CorruptPayload {
            session_sequence: Some(sequence),
            reason: error.to_string(),
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
mod tests;
