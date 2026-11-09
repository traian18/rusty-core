//! Compaction lifecycle events kept for the open session, so a subscriber that missed some (a
//! lagging broadcast, a finish that arrived before its start) can recover them.
//!
//! Runners retain an envelope once it is committed and before either lossy broadcast (the
//! agent's own channel and the session bus), so neither can lose it for good. A rejected strict
//! commit is never published and so never retained. The events are not durable: they are
//! released when the session closes, are not part of `events_since`, and are not restored when a
//! session is reopened.

use std::sync::{Arc, Mutex};

use rusty_protocol::events::{AgentEvent, AgentEventEnvelope};

use crate::locks::MutexExt;

/// The open session's retained lifecycle envelopes, in retention order.
#[derive(Clone, Default)]
pub struct LifecycleRetention(Arc<Mutex<Retained>>);

#[derive(Default)]
struct Retained {
    envelopes: Vec<AgentEventEnvelope>,
    closed: bool,
}

/// What a recovery call returns.
#[derive(Debug, Clone)]
pub enum LifecycleRecovery {
    /// The retained lifecycle envelopes after the cursor, and the cursor to ask from next time
    /// (inclusive high-water mark: everything up to it has now been returned).
    Recovered {
        envelopes: Vec<AgentEventEnvelope>,
        high_water: u64,
    },
    /// The session closed: nothing can be recovered any more.
    Unavailable,
}

/// Whether `event` is kept for recovery.
pub fn is_lifecycle(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::CompactionStarted { .. } | AgentEvent::CompactionFinished { .. }
    )
}

impl LifecycleRetention {
    /// Keeps `envelope` when it is a lifecycle event of a still-open session.
    pub fn retain(&self, envelope: &AgentEventEnvelope) {
        if !is_lifecycle(&envelope.event) {
            return;
        }
        let mut retained = self.0.lock_or_recover();
        if !retained.closed {
            retained.envelopes.push(envelope.clone());
        }
    }

    /// The envelopes retained after the first `cursor` ones (`0`: from the session's start).
    pub fn recover(&self, cursor: u64) -> LifecycleRecovery {
        let retained = self.0.lock_or_recover();
        if retained.closed {
            return LifecycleRecovery::Unavailable;
        }
        let from = usize::try_from(cursor)
            .unwrap_or(usize::MAX)
            .min(retained.envelopes.len());
        LifecycleRecovery::Recovered {
            envelopes: retained.envelopes[from..].to_vec(),
            high_water: retained.envelopes.len() as u64,
        }
    }

    /// Releases everything retained; later recoveries are unavailable.
    pub fn close(&self) {
        let mut retained = self.0.lock_or_recover();
        retained.closed = true;
        retained.envelopes = Vec::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_protocol::events::{CompactionType, EventVisibility};
    use rusty_protocol::ids::{AgentId, EventId, RequestId, SessionId, Timestamp};

    fn envelope(event: AgentEvent) -> AgentEventEnvelope {
        AgentEventEnvelope {
            event_id: EventId::new(),
            session_id: SessionId::new(),
            agent_id: AgentId::new(),
            parent_agent_id: None,
            run_id: None,
            agent_sequence: 0,
            session_sequence: None,
            timestamp: Timestamp::now(),
            visibility: EventVisibility::User,
            event,
        }
    }

    fn started() -> AgentEvent {
        AgentEvent::CompactionStarted {
            request_id: RequestId::new(),
            request_start_order: 0,
            operation_id: "op".into(),
            compaction_type: CompactionType::Standard,
            before_estimated_input_tokens: None,
            compaction_started: true,
            compaction_ended: false,
        }
    }

    /// The recovered event ids and high-water mark, or `None` when unavailable.
    fn ids(recovery: LifecycleRecovery) -> Option<(Vec<EventId>, u64)> {
        match recovery {
            LifecycleRecovery::Recovered {
                envelopes,
                high_water,
            } => Some((envelopes.iter().map(|e| e.event_id).collect(), high_water)),
            LifecycleRecovery::Unavailable => None,
        }
    }

    #[test]
    fn only_lifecycle_events_are_kept_and_recovered_after_the_cursor() {
        let retention = LifecycleRetention::default();
        let first = envelope(started());
        retention.retain(&first);
        retention.retain(&envelope(AgentEvent::RunStarted {
            run_id: rusty_protocol::ids::RunId::new(),
        }));
        let second = envelope(started());
        retention.retain(&second);
        assert_eq!(
            ids(retention.recover(0)),
            Some((vec![first.event_id, second.event_id], 2))
        );
        assert_eq!(ids(retention.recover(1)), Some((vec![second.event_id], 2)));
        assert_eq!(ids(retention.recover(9)), Some((vec![], 2)));
    }

    #[test]
    fn a_closed_session_releases_its_events_and_cannot_recover() {
        let retention = LifecycleRetention::default();
        retention.retain(&envelope(started()));
        retention.close();
        retention.retain(&envelope(started()));
        assert!(ids(retention.recover(0)).is_none());
    }
}
