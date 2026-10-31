use super::*;
use crate::commit::{DurabilityPolicy, SessionCommitter};
use crate::store::{SessionStore, StoreError};
use rusty_protocol::commands::AgentStatus;
use rusty_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
use rusty_protocol::ids::{AgentId, EventId, RunId};
use rusty_protocol::ids::{SessionId, Timestamp};
use std::sync::Arc;

fn durable_event(session_id: SessionId) -> AgentEventEnvelope {
    AgentEventEnvelope {
        event_id: EventId::new(),
        session_id,
        agent_id: AgentId::new(),
        parent_agent_id: None,
        run_id: Some(RunId::new()),
        agent_sequence: 0,
        session_sequence: None,
        timestamp: Timestamp::now(),
        visibility: EventVisibility::User,
        event: AgentEvent::StateChanged {
            from: AgentStatus::Idle,
            to: AgentStatus::PreparingContext,
        },
    }
}

/// M2 crash fixture: a `Strict`-policy commit whose durable append fails
/// must not advance the durably observable state. A fresh committer
/// built over the same backing store after the fault clears (simulating
/// a process restart) must resume at the sequence the store actually
/// has, without a gap and without silently losing the failed event.
#[tokio::test]
async fn strict_policy_failed_append_does_not_leave_a_persisted_gap() {
    let backing = Arc::new(MemoryStore::new());
    let faulty = Arc::new(FaultInjectingStore::new(backing.clone()));
    let session = SessionId::new();

    faulty.set_fault_hook(|op, call_count| {
        if op == "append" && call_count == 1 {
            Some(StoreError::Backend("simulated crash mid-write".into()))
        } else {
            None
        }
    });

    let committer = SessionCommitter::new(faulty.clone(), session)
        .with_durability_policy(DurabilityPolicy::Strict);
    let result = committer.commit(durable_event(session)).await;
    assert!(
        result.is_err(),
        "strict policy must surface the store failure rather than publish"
    );

    // Nothing was actually persisted by the failed attempt.
    assert_eq!(backing.current_sequence(session).await.unwrap_or(0), 0);

    // Simulate a restart: clear the fault and build a fresh committer
    // over the same backing store.
    faulty.clear_fault_hook();
    let restarted =
        SessionCommitter::new(faulty, session).with_durability_policy(DurabilityPolicy::Strict);
    let committed = restarted
        .commit(durable_event(session))
        .await
        .expect("commit")
        .expect("published");

    // The sequencer must resume from the store's true durable state
    // (sequence 0), so the retried event lands at sequence 1 with no
    // gap and no duplicate.
    assert_eq!(committed.envelope.session_sequence, Some(1));
    let stored = backing.load_session(session).await.expect("load");
    assert_eq!(stored.events.len(), 1);
}

/// M2: `BestEffort` policy must let execution continue but must mark
/// the event `degraded`, and the degraded event must not silently
/// appear as durably persisted.
#[tokio::test]
async fn best_effort_policy_marks_degraded_and_does_not_fabricate_durability() {
    let backing = Arc::new(MemoryStore::new());
    let faulty = Arc::new(FaultInjectingStore::new(backing.clone()));
    let session = SessionId::new();

    faulty.set_fault_hook(|op, _| {
        if op == "append" {
            Some(StoreError::Backend("simulated disk full".into()))
        } else {
            None
        }
    });

    let committer =
        SessionCommitter::new(faulty, session).with_durability_policy(DurabilityPolicy::BestEffort);
    let committed = committer
        .commit(durable_event(session))
        .await
        .expect("commit does not error under BestEffort")
        .expect("still published");

    assert!(
        committed.degraded,
        "a failed append must be marked degraded"
    );
    assert_eq!(
        backing.current_sequence(session).await.unwrap_or(0),
        0,
        "a degraded event must not appear durably persisted in the backing store"
    );
}

/// M2: a `save_snapshot` failure must not corrupt or lose prior durable
/// events; the store's event history remains queryable independent of
/// checkpoint failures.
#[tokio::test]
async fn snapshot_failure_does_not_lose_durable_events() {
    let backing = Arc::new(MemoryStore::new());
    let faulty = Arc::new(FaultInjectingStore::new(backing.clone()));
    let session = SessionId::new();

    let committer = SessionCommitter::new(faulty.clone(), session);
    committer
        .commit(durable_event(session))
        .await
        .expect("commit")
        .expect("published");

    faulty.set_fault_hook(|op, _| {
        if op == "save_snapshot" {
            Some(StoreError::Backend("simulated checkpoint failure".into()))
        } else {
            None
        }
    });

    let snapshot = test_snapshot(session, 1);
    let result = faulty.save_snapshot(snapshot).await;
    assert!(result.is_err());

    let stored = backing.load_session(session).await.expect("load");
    assert_eq!(
        stored.events.len(),
        1,
        "prior durable events must survive a checkpoint failure"
    );
    assert!(
        stored.snapshot.is_none(),
        "the failed snapshot must not appear saved"
    );
}
