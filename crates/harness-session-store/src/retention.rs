//! Retention and compaction planning (RC-305).
//!
//! The durable event log is append-only; snapshots are additive. "Compaction"
//! therefore means: **take a new snapshot** so future replays only re-apply
//! the events after it, and — only when the host explicitly opts in —
//! **prune** the events the snapshot now covers.
//!
//! # Invariants
//!
//! - Retention never destroys replay or audit prerequisites unless the host
//!   consciously opts in: pruning requires an existing snapshot at or above
//!   the prune point (verified by [`prune_plan`]), and
//!   [`SessionStore::prune_events_before`](crate::store::SessionStore::prune_events_before)
//!   defaults to *rejecting* the call.
//! - [`plan_compaction`] is a pure function: it only computes a plan.
//!   Applying it (writing the snapshot, pruning events) is the caller's
//!   job, using the plan's [`snapshot_sequence`](CompactionPlan::snapshot_sequence).
//! - Compaction lineage is recorded on the new snapshot's
//!   [`DurableSessionMetadata`](crate::store::DurableSessionMetadata)
//!   (`compacted` / `compaction_generation`), so a compacted snapshot is
//!   never mistaken for a plain checkpoint.

use harness_protocol::ids::SessionId;

use crate::store::{DurableSessionSnapshot, StoredSession};

/// Retention policy governing when a session should be compacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Compact when the durable events *after* the latest snapshot exceed
    /// this many. `0` disables count-based compaction.
    pub max_trailing_events: u64,
    /// When `true`, compaction only writes a new snapshot and never prunes
    /// the covered events (full replay/audit history is preserved). When
    /// `false`, the plan additionally proposes pruning the covered events
    /// (see [`CompactionPlan::prune_through`]).
    pub keep_full_history: bool,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_trailing_events: 1024,
            keep_full_history: true,
        }
    }
}

/// A pure compaction decision for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionPlan {
    /// The session being planned.
    pub session_id: SessionId,
    /// How many durable events trail the latest snapshot.
    pub trailing_events: u64,
    /// Whether a snapshot should be written now.
    pub should_compact: bool,
    /// The durable sequence the new snapshot should be taken at (the last
    /// event it covers). `0` when no compaction is needed.
    pub snapshot_sequence: u64,
    /// The sequence through which covered events may be pruned when
    /// `keep_full_history` is `false`. `0` when pruning is not proposed.
    pub prune_through: u64,
    /// The next compaction generation (latest generation + 1).
    pub next_generation: u64,
}

impl CompactionPlan {
    /// Builds an empty plan (no compaction needed).
    fn idle(session_id: SessionId, next_generation: u64) -> Self {
        Self {
            session_id,
            trailing_events: 0,
            should_compact: false,
            snapshot_sequence: 0,
            prune_through: 0,
            next_generation,
        }
    }
}

/// Computes the compaction plan for `stored` under `policy` — pure, no I/O.
///
/// The plan proposes a snapshot at the last durable event's sequence when
/// the trailing event count exceeds `policy.max_trailing_events`. When
/// `policy.keep_full_history` is `false` and a snapshot exists, the plan
/// also proposes pruning through the *previous* snapshot's sequence (so the
/// current snapshot — the one being written — always remains the recovery
/// anchor for the pruned range).
pub fn plan_compaction(stored: &StoredSession, policy: &RetentionPolicy) -> CompactionPlan {
    let generation = stored
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.metadata.compaction_generation)
        .unwrap_or(0);

    let trailing: Vec<u64> = stored
        .events
        .iter()
        .filter_map(|event| event.session_sequence)
        .collect();
    let trailing_events = trailing.len() as u64;
    let last_sequence = trailing.last().copied().unwrap_or(0);

    if policy.max_trailing_events == 0 || trailing_events <= policy.max_trailing_events {
        return CompactionPlan::idle(stored.session_id, generation.saturating_add(1));
    }

    let mut plan = CompactionPlan {
        session_id: stored.session_id,
        trailing_events,
        should_compact: true,
        snapshot_sequence: last_sequence,
        prune_through: 0,
        next_generation: generation.saturating_add(1),
    };

    if !policy.keep_full_history {
        // Prune only events covered by a snapshot that will exist at or above
        // the prune point. The previous snapshot's sequence is the safe
        // anchor; events above it stay for replay against the new snapshot.
        if let Some(previous) = stored
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.session_sequence)
        {
            plan.prune_through = previous;
        }
    }
    plan
}

/// Verifies that pruning through `sequence` keeps a valid replay anchor.
///
/// Returns the first durable sequence that must survive (`sequence + 1`), or
/// `Err` when no snapshot covers `sequence` (pruning would destroy the
/// replay/audit prerequisites).
pub fn prune_plan(stored: &StoredSession, sequence: u64) -> Result<u64, &'static str> {
    let snapshot_sequence = stored
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.session_sequence)
        .ok_or("cannot prune without a snapshot: replay anchor would be lost")?;
    if snapshot_sequence < sequence {
        return Err("cannot prune beyond the latest snapshot's sequence");
    }
    Ok(sequence.saturating_add(1))
}

/// Marks `snapshot` as a compaction checkpoint, advancing its lineage.
///
/// The snapshot's metadata is stamped with `compacted = true` and the next
/// generation number; `session_sequence` is left untouched (the caller sets
/// it from the compaction plan).
pub fn mark_compacted(mut snapshot: DurableSessionSnapshot) -> DurableSessionSnapshot {
    snapshot.metadata.compacted = true;
    snapshot.metadata.compaction_generation =
        snapshot.metadata.compaction_generation.saturating_add(1);
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{DurableSessionEvent, DurableSessionMetadata};
    use harness_protocol::commands::AgentStatus;
    use harness_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
    use harness_protocol::ids::{AgentId, EventId, RunId, SessionId, Timestamp};

    fn event(session: SessionId, seq: u64) -> DurableSessionEvent {
        DurableSessionEvent {
            session_sequence: Some(seq),
            envelope: AgentEventEnvelope {
                event_id: EventId::new(),
                session_id: session,
                agent_id: AgentId::new(),
                parent_agent_id: None,
                run_id: Some(RunId::new()),
                agent_sequence: seq,
                session_sequence: Some(seq),
                timestamp: Timestamp::now(),
                visibility: EventVisibility::User,
                event: AgentEvent::StateChanged {
                    from: AgentStatus::Idle,
                    to: AgentStatus::PreparingContext,
                },
            },
        }
    }

    fn snapshot(session: SessionId, seq: u64, generation: u64) -> DurableSessionSnapshot {
        DurableSessionSnapshot {
            session_id: session,
            root_agent_id: AgentId::new(),
            agents: Vec::new(),
            session_sequence: seq,
            timestamp: Timestamp::now(),
            schema_version: crate::version::SCHEMA_VERSION,
            metadata: DurableSessionMetadata {
                compaction_generation: generation,
                ..Default::default()
            },
        }
    }

    #[test]
    fn under_threshold_produces_idle_plan() {
        let session = SessionId::new();
        let stored = StoredSession {
            session_id: session,
            snapshot: Some(snapshot(session, 2, 0)),
            events: vec![event(session, 3)],
        };
        let plan = plan_compaction(
            &stored,
            &RetentionPolicy {
                max_trailing_events: 1024,
                keep_full_history: true,
            },
        );
        assert!(!plan.should_compact);
    }

    #[test]
    fn over_threshold_proposes_snapshot_at_last_sequence() {
        let session = SessionId::new();
        let stored = StoredSession {
            session_id: session,
            snapshot: Some(snapshot(session, 1, 0)),
            events: vec![event(session, 2), event(session, 3), event(session, 4)],
        };
        let plan = plan_compaction(
            &stored,
            &RetentionPolicy {
                max_trailing_events: 2,
                keep_full_history: true,
            },
        );
        assert!(plan.should_compact);
        assert_eq!(plan.snapshot_sequence, 4);
        assert_eq!(plan.prune_through, 0, "full-history keeps everything");
        assert_eq!(plan.next_generation, 1);
    }

    #[test]
    fn prune_mode_anchors_on_previous_snapshot() {
        let session = SessionId::new();
        let stored = StoredSession {
            session_id: session,
            snapshot: Some(snapshot(session, 3, 0)),
            events: vec![event(session, 4), event(session, 5)],
        };
        let plan = plan_compaction(
            &stored,
            &RetentionPolicy {
                max_trailing_events: 1,
                keep_full_history: false,
            },
        );
        assert!(plan.should_compact);
        assert_eq!(plan.snapshot_sequence, 5);
        assert_eq!(
            plan.prune_through, 3,
            "pruning never crosses the previous snapshot's sequence"
        );
    }

    #[test]
    fn prune_plan_requires_a_snapshot_anchor() {
        let session = SessionId::new();
        let stored = StoredSession {
            session_id: session,
            snapshot: None,
            events: vec![event(session, 1)],
        };
        assert!(prune_plan(&stored, 1).is_err());
    }

    #[test]
    fn prune_plan_rejects_pruning_beyond_snapshot() {
        let session = SessionId::new();
        let stored = StoredSession {
            session_id: session,
            snapshot: Some(snapshot(session, 2, 0)),
            events: vec![event(session, 3)],
        };
        assert!(prune_plan(&stored, 3).is_err());
        assert_eq!(prune_plan(&stored, 2).expect("anchored"), 3);
    }

    #[test]
    fn mark_compacted_advances_lineage() {
        let session = SessionId::new();
        let stamped = mark_compacted(snapshot(session, 5, 0));
        assert!(stamped.metadata.compacted);
        assert_eq!(stamped.metadata.compaction_generation, 1);
    }
}
