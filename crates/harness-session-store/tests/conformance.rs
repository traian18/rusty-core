//! Shared `SessionStore` conformance suite (Tasks 7.3/7.4 acceptance criteria).
//!
//! The same battery of contract checks runs against **both** concrete store
//! implementations — [`SqliteSessionStore`] and [`JsonlSessionStore`] — via
//! [`conformance_suite`], so a behavioral difference between the two fails
//! loudly at the shared boundary instead of being papered over by
//! implementation-specific tests.
//!
//! The suite covers the cross-implementation contract from spec §59/§71:
//!
//! - **round trip** — append N events + a snapshot, reload, and confirm the
//!   reconstructed [`StoredSession`] matches **exactly** (the acceptance test
//!   from Tasks 7.3/7.4, and the strongest statement of the "snapshot + event
//!   restoration" contract);
//! - loading a session with no snapshot returns the full event log in
//!   session-sequence order;
//! - an unknown session loads as [`StoreError::NotFound`];
//! - a later snapshot replaces an earlier one;
//! - concurrent appends from many tasks land intact (both stores serialize
//!   writes through a single-writer model).
//!
//! Implementation-specific guarantees (WAL mode confirmation, crash recovery,
//! duplicate-sequence rejection, unsequenced-event assignment, cross-instance
//! durability) are covered by each implementation's in-module tests and are
//! deliberately **not** part of the shared suite, since the two backends
//! legitimately differ there.

use std::path::PathBuf;
use std::sync::Arc;

use harness_protocol::commands::AgentStatus;
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
use harness_protocol::ids::{AgentId, EventId, MessageId, RunId, SessionId, Timestamp};
use harness_session_store::{
    DurableSessionEvent, DurableSessionSnapshot, JsonlSessionStore, SessionStore,
    SqliteSessionStore, StoredSession, StoreError,
};

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// A fully-qualified, sequenced envelope for testing.
///
/// `agent_sequence` and `session_sequence` mirror `seq` so both ordering
/// dimensions are deterministic, and the state transition varies per sequence
/// so an envelope-swapping bug would be caught by the exact-match comparison.
fn envelope(session: SessionId, seq: u64) -> AgentEventEnvelope {
    let (from, to) = match seq % 3 {
        0 => (AgentStatus::Idle, AgentStatus::PreparingContext),
        1 => (AgentStatus::PreparingContext, AgentStatus::WaitingForBackend),
        _ => (AgentStatus::WaitingForBackend, AgentStatus::Executing),
    };
    AgentEventEnvelope {
        event_id: EventId::new(),
        session_id: session,
        agent_id: AgentId::new(),
        parent_agent_id: None,
        run_id: Some(RunId::new()),
        agent_sequence: seq,
        session_sequence: Some(seq),
        timestamp: Timestamp::now(),
        visibility: EventVisibility::User,
        event: AgentEvent::StateChanged { from, to },
    }
}

/// A durable event with the given session sequence.
fn event(session: SessionId, seq: u64) -> DurableSessionEvent {
    DurableSessionEvent {
        session_sequence: Some(seq),
        envelope: envelope(session, seq),
    }
}

/// A minimal snapshot at the given session sequence.
fn snapshot(session: SessionId, seq: u64) -> DurableSessionSnapshot {
    DurableSessionSnapshot {
        session_id: session,
        root_agent_id: AgentId::new(),
        agents: Vec::new(),
        session_sequence: seq,
        timestamp: Timestamp::now(),
    }
}

/// Structural equality for [`StoredSession`]s via their JSON projection.
///
/// `serde_json::Value` object keys are sorted, so the comparison is immune to
/// `HashMap` iteration order inside snapshot agent state and to JSON key-order
/// noise, while still asserting full field-by-field equality.
fn assert_stored_sessions_match(expected: &StoredSession, actual: &StoredSession) {
    let expected = serde_json::to_value(expected).expect("serialize expected stored session");
    let actual = serde_json::to_value(actual).expect("serialize loaded stored session");
    assert_eq!(
        expected, actual,
        "reconstructed StoredSession must match exactly (snapshot + trailing events)"
    );
}

/// Creates a unique scratch directory for one test run and returns the
/// SQLite database path inside it.
fn temp_db(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "harness-session-store-conformance-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp store dir");
    dir.join("store.db")
}

/// Creates a unique scratch directory for one test run.
fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "harness-session-store-conformance-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp store dir");
    dir
}

// ---------------------------------------------------------------------------
// Shared conformance checks
// ---------------------------------------------------------------------------

/// Round-trip acceptance test (Tasks 7.3/7.4): append N events + a snapshot,
/// reload, and confirm the reconstructed `StoredSession` matches exactly —
/// i.e. the saved snapshot plus precisely the events appended after it, with
/// every envelope field intact.
async fn round_trip_reconstructed_session_matches_exactly(store: Arc<dyn SessionStore>) {
    const N: u64 = 5; // number of events appended
    const SNAPSHOT_AT: u64 = 3; // session sequence the snapshot is taken at (SNAPSHOT_AT < N)

    let session = SessionId::new();
    let mut appended: Vec<DurableSessionEvent> = Vec::new();
    for seq in 1..=N {
        let ev = event(session, seq);
        appended.push(ev.clone());
        store.append(ev).await.expect("append event");
    }

    let saved_snapshot = snapshot(session, SNAPSHOT_AT);
    store
        .save_snapshot(saved_snapshot.clone())
        .await
        .expect("save snapshot");

    let loaded = store.load_session(session).await.expect("load session");
    assert_eq!(loaded.session_id, session);

    let expected = StoredSession {
        session_id: session,
        snapshot: Some(saved_snapshot),
        events: appended
            .into_iter()
            .filter(|event| event.session_sequence.is_some_and(|seq| seq > SNAPSHOT_AT))
            .collect(),
    };
    assert_stored_sessions_match(&expected, &loaded);
}

/// With no snapshot on file, `load_session` returns the full event log in
/// session-sequence order.
async fn load_returns_all_events_when_no_snapshot(store: Arc<dyn SessionStore>) {
    let session = SessionId::new();
    for seq in 1..=3 {
        store.append(event(session, seq)).await.expect("append event");
    }

    let stored = store.load_session(session).await.expect("load session");
    assert!(stored.snapshot.is_none(), "no snapshot was saved");
    let sequences: Vec<u64> = stored
        .events
        .iter()
        .map(|event| event.session_sequence.expect("sequenced event"))
        .collect();
    assert_eq!(
        sequences,
        vec![1, 2, 3],
        "events come back in session-sequence order"
    );
    for event in &stored.events {
        assert_eq!(event.envelope.session_id, session);
        assert!(matches!(event.envelope.event, AgentEvent::StateChanged { .. }));
    }
}

/// An unknown session loads as `StoreError::NotFound`.
async fn load_missing_session_is_not_found(store: Arc<dyn SessionStore>) {
    let error = store
        .load_session(SessionId::new())
        .await
        .expect_err("no stored data exists for an unknown session");
    assert!(matches!(error, StoreError::NotFound(_)));
}

/// The store boundary itself rejects ephemeral events, so a caller cannot
/// bypass the runtime's durability filter and accidentally persist streaming
/// deltas.
async fn ephemeral_events_are_rejected(store: Arc<dyn SessionStore>) {
    let session = SessionId::new();
    let mut ephemeral = event(session, 1);
    ephemeral.envelope.event = AgentEvent::AssistantTextDelta {
        message_id: MessageId::new(),
        delta: "partial".into(),
    };

    let error = store
        .append(ephemeral)
        .await
        .expect_err("ephemeral event must not reach durable storage");
    assert!(matches!(error, StoreError::InvalidState(_)));
    assert!(matches!(
        store.load_session(session).await,
        Err(StoreError::NotFound(id)) if id == session
    ));
}

/// Saving a second snapshot for the same session replaces the first: the
/// reloaded snapshot is the latest one and its earlier events are absorbed.
async fn later_snapshot_replaces_earlier(store: Arc<dyn SessionStore>) {
    let session = SessionId::new();
    store.append(event(session, 1)).await.expect("append event");
    store
        .save_snapshot(snapshot(session, 1))
        .await
        .expect("save snapshot 1");
    store.append(event(session, 2)).await.expect("append event");
    store
        .save_snapshot(snapshot(session, 2))
        .await
        .expect("save snapshot 2");

    let stored = store.load_session(session).await.expect("load session");
    assert_eq!(stored.session_id, session);
    let latest = stored.snapshot.expect("snapshot present");
    assert_eq!(
        latest.session_sequence, 2,
        "the latest snapshot replaces any earlier one"
    );
    assert_eq!(
        stored.events.len(),
        0,
        "both events are captured by the latest snapshot"
    );
}

/// Appends from many concurrent tasks must all land as complete, intact
/// records in session-sequence order (both stores serialize writes through a
/// single-writer model).
async fn concurrent_appends_are_serialized_and_intact(store: Arc<dyn SessionStore>) {
    const CONCURRENCY: u64 = 32;
    let session = SessionId::new();

    let mut handles = Vec::new();
    for seq in 0..CONCURRENCY {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            store
                .append(event(session, seq))
                .await
                .expect("concurrent append");
        }));
    }
    for handle in handles {
        handle.await.expect("join append task");
    }

    let stored = store.load_session(session).await.expect("load session");
    assert!(stored.snapshot.is_none());
    let sequences: Vec<u64> = stored
        .events
        .iter()
        .map(|event| event.session_sequence.expect("sequenced event"))
        .collect();
    assert_eq!(
        sequences,
        (0..CONCURRENCY).collect::<Vec<u64>>(),
        "every append landed exactly once, in session-sequence order"
    );
    assert!(
        stored
            .events
            .iter()
            .all(|event| event.envelope.event_id.to_string().len() == 36),
        "every reconstructed envelope is a complete, intact event"
    );
}

/// Runs every shared conformance check against a live store instance.
async fn conformance_suite(store: Arc<dyn SessionStore>) {
    round_trip_reconstructed_session_matches_exactly(store.clone()).await;
    load_returns_all_events_when_no_snapshot(store.clone()).await;
    load_missing_session_is_not_found(store.clone()).await;
    ephemeral_events_are_rejected(store.clone()).await;
    later_snapshot_replaces_earlier(store.clone()).await;
    concurrent_appends_are_serialized_and_intact(store).await;
}

// ---------------------------------------------------------------------------
// Entry points: one per implementation, both running the shared suite
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sqlite_session_store_passes_conformance_suite() {
    let store: Arc<dyn SessionStore> = Arc::new(
        SqliteSessionStore::open(temp_db("sqlite-conformance")).expect("open sqlite store"),
    );
    conformance_suite(store).await;
}

#[tokio::test]
async fn jsonl_session_store_passes_conformance_suite() {
    let store: Arc<dyn SessionStore> =
        Arc::new(JsonlSessionStore::new(temp_dir("jsonl-conformance")));
    conformance_suite(store).await;
}
