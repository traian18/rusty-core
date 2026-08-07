//! RC-300 end-to-end story tests: authoritative commit → snapshot → restore
//! validation → host dependency resolution → retention/diagnostics.
//!
//! These tests exercise the full truthfulness chain across the public store
//! API surface — the same chain a runtime restore follows — against
//! [`harness_session_store::testing::MemoryStore`] (deterministic, no I/O)
//! and against the real [`JsonlSessionStore`] / [`SqliteSessionStore`] where
//! the behavior is cross-implementation.

use std::sync::Arc;

use harness_protocol::commands::AgentStatus;
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
use harness_protocol::ids::{AgentId, EventId, RunId, SessionId, Timestamp};
use harness_session_store::testing::MemoryStore;
use harness_session_store::{
    assess_restore, diagnose_store, migrate_snapshot, plan_compaction, validate_trailing_replay,
    DependencyKind, DurabilityPolicy, GapPolicy, HostDependencyResolver, JsonlSessionStore,
    PermissiveResolver, ReplayError, RestorePolicy, RetentionPolicy, SessionCommitter,
    SessionStore, SqliteSessionStore, StoredSession, SCHEMA_VERSION,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn envelope_for(session_id: SessionId, seq: u64, event: AgentEvent) -> AgentEventEnvelope {
    AgentEventEnvelope {
        event_id: EventId::new(),
        session_id,
        agent_id: AgentId::new(),
        parent_agent_id: None,
        run_id: Some(RunId::new()),
        agent_sequence: seq,
        session_sequence: None,
        timestamp: Timestamp::now(),
        visibility: EventVisibility::User,
        event,
    }
}

fn state_changed(session_id: SessionId, seq: u64, to: AgentStatus) -> AgentEventEnvelope {
    envelope_for(
        session_id,
        seq,
        AgentEvent::StateChanged {
            from: AgentStatus::Idle,
            to,
        },
    )
}

fn completed(session_id: SessionId, seq: u64) -> AgentEventEnvelope {
    envelope_for(
        session_id,
        seq,
        AgentEvent::Completed {
            outcome: harness_protocol::events::AgentOutcome::Success,
        },
    )
}

// ---------------------------------------------------------------------------
// Story: commit → checkpoint → restore validation → resolver
// ---------------------------------------------------------------------------

/// The full RC-300 chain: events flow through the committer (final sequences
/// in the store), a checkpoint is saved, a *fresh* committer resumes after
/// the last durable event, trailing replay validates the stream, and the
/// snapshot migrates/stamps the current schema version.
#[tokio::test]
async fn commit_checkpoint_restore_chain_is_truthful() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new();

    // ── Phase 1: an authoritative session writes events + a checkpoint ──
    {
        let committer = Arc::new(SessionCommitter::new(store.clone(), session));
        let first = committer
            .commit(state_changed(session, 1, AgentStatus::PreparingContext))
            .await
            .expect("commit")
            .expect("published");
        let second = committer
            .commit(completed(session, 2))
            .await
            .expect("commit")
            .expect("published");
        assert_eq!(first.envelope.session_sequence, Some(1));
        assert_eq!(second.envelope.session_sequence, Some(2));

        // A checkpoint at the last committed sequence.
        store
            .save_snapshot(harness_session_store::testing::test_snapshot(session, 2))
            .await
            .expect("save checkpoint");
    }

    // ── Phase 2: restore validation over what a store would load ──
    let stored = store.load_session(session).await.expect("load");
    assert_eq!(
        stored.snapshot.as_ref().map(|s| s.session_sequence),
        Some(2)
    );
    let trailing = validate_trailing_replay(&stored, GapPolicy::Strict).expect("valid replay");
    assert_eq!(trailing.len(), 0, "the checkpoint covers every event");

    // ── Phase 3: a fresh committer resumes after the last durable event ──
    let restarted = Arc::new(SessionCommitter::new(store.clone(), session));
    let next = restarted
        .commit(state_changed(session, 3, AgentStatus::WaitingForBackend))
        .await
        .expect("commit")
        .expect("published");
    assert_eq!(
        next.envelope.session_sequence,
        Some(3),
        "a restarted session continues after its checkpoint"
    );

    // ── Phase 4: the checkpoint is versioned and migratable ──
    let loaded = store.load_session(session).await.expect("load");
    let snapshot = loaded.snapshot.expect("snapshot present");
    assert_eq!(snapshot.schema_version, SCHEMA_VERSION);
    let migrated = migrate_snapshot(snapshot).expect("already current, no-op");
    assert_eq!(migrated.schema_version, SCHEMA_VERSION);
}

/// Strict durability: when the store rejects a durable append, the strict
/// policy surfaces a typed error and the event is never published.
#[tokio::test]
async fn strict_policy_surfaces_failed_persist_truthfully() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new();
    let committer = Arc::new(SessionCommitter::new(store.clone(), session));

    // Prime the session, then make the store fail.
    committer
        .commit(state_changed(session, 1, AgentStatus::PreparingContext))
        .await
        .expect("prime");
    store.set_fail_appends(true);

    let result = committer.commit(completed(session, 2)).await;
    assert!(
        matches!(
            result,
            Err(harness_session_store::CommitError::Store(
                harness_session_store::StoreError::Backend(_)
            ))
        ),
        "strict policy returns the typed persistence failure"
    );

    // The store has no trace of the failed event — nothing was called durable
    // prematurely.
    store.set_fail_appends(false);
    let stored = store.load_session(session).await.expect("load");
    assert_eq!(stored.events.len(), 1);
}

/// Best-effort durability: the event is published but explicitly degraded.
#[tokio::test]
async fn best_effort_policy_marks_degraded_publication() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new();
    let committer = Arc::new(
        SessionCommitter::new(store.clone(), session)
            .with_durability_policy(DurabilityPolicy::BestEffort),
    );
    store.set_fail_appends(true);

    let committed = committer
        .commit(completed(session, 1))
        .await
        .expect("best effort succeeds")
        .expect("published");
    assert!(committed.degraded, "the event is tagged never-durable");
    assert_eq!(committed.envelope.session_sequence, Some(1));
}

/// Restore never silently substitutes a fake workspace: a snapshot that
/// recorded a workspace identity rejects a strict restore when the host
/// cannot resolve it.
#[tokio::test]
async fn strict_restore_rejects_missing_workspace_dependency() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new();

    let committer = Arc::new(SessionCommitter::new(store.clone(), session));
    committer
        .commit(state_changed(session, 1, AgentStatus::PreparingContext))
        .await
        .expect("commit");

    // Snapshot under a workspace identity.
    let mut snapshot = harness_session_store::testing::test_snapshot(session, 1);
    snapshot.metadata.workspace_identity = Some("/srv/prod".into());
    store
        .save_snapshot(snapshot)
        .await
        .expect("save checkpoint");

    // The host's permissive resolver reports the workspace as missing; a
    // strict policy rejects the restore.
    let stored = store.load_session(session).await.expect("load");
    let snapshot = stored.snapshot.expect("snapshot");
    let report = PermissiveResolver
        .resolve(session, &snapshot.metadata)
        .await;
    assert_eq!(report.missing.len(), 1);
    assert_eq!(report.missing[0].kind, DependencyKind::Workspace);
    assert_eq!(report.missing[0].id, "/srv/prod");

    let error = assess_restore(&report, RestorePolicy::RejectMissing).expect_err("strict restore");
    assert!(matches!(
        error,
        harness_session_store::RestoreError::MissingDependencies { count: 1, .. }
    ));
}

/// Replay rejects a corrupted durable stream before it is applied.
#[tokio::test]
async fn trailing_replay_rejects_corrupt_stream() {
    let session = SessionId::new();
    let mut stored = StoredSession {
        session_id: session,
        snapshot: None,
        // Duplicate sequences make the stream corrupt.
        events: vec![
            harness_session_store::DurableSessionEvent {
                session_sequence: Some(1),
                envelope: {
                    let mut envelope = state_changed(session, 1, AgentStatus::PreparingContext);
                    envelope.session_sequence = Some(1);
                    envelope
                },
            },
            harness_session_store::DurableSessionEvent {
                session_sequence: Some(1),
                envelope: {
                    let mut envelope = completed(session, 2);
                    envelope.session_sequence = Some(1);
                    envelope
                },
            },
        ],
    };
    let error = validate_trailing_replay(&stored, GapPolicy::Strict).expect_err("corrupt");
    assert!(matches!(error, ReplayError::DuplicateSequence(1)));

    // A version 2 snapshot (from a newer build) is rejected up-front.
    stored.snapshot = Some(harness_session_store::DurableSessionSnapshot {
        session_id: session,
        root_agent_id: AgentId::new(),
        agents: Vec::new(),
        session_sequence: 1,
        timestamp: Timestamp::now(),
        schema_version: SCHEMA_VERSION + 1,
        metadata: Default::default(),
    });
    let error = validate_trailing_replay(&stored, GapPolicy::Strict).expect_err("future version");
    assert!(matches!(error, ReplayError::FutureSnapshotVersion { .. }));
}

/// Retention planning is pure and preserves the replay anchor; pruning the
/// covered events through the SQLite store keeps trailing replay valid.
#[tokio::test]
async fn retention_plan_preserves_replay_anchor() {
    let dir = std::env::temp_dir().join(format!(
        "harness-rc300-retention-{}-{}",
        std::process::id(),
        Timestamp::now().timestamp_millis()
    ));
    let store =
        Arc::new(SqliteSessionStore::open(dir.join("store.db")).expect("open sqlite store"));
    let session = SessionId::new();

    let committer = Arc::new(SessionCommitter::new(store.clone(), session));
    for seq in 1..=5 {
        committer
            .commit(state_changed(session, seq, AgentStatus::PreparingContext))
            .await
            .expect("commit");
    }
    store
        .save_snapshot(harness_session_store::testing::test_snapshot(session, 5))
        .await
        .expect("checkpoint");

    // A compacted session keeps full history by default; pruning is explicit.
    let stored = store.load_session(session).await.expect("load");
    let plan = plan_compaction(
        &stored,
        &RetentionPolicy {
            max_trailing_events: 2,
            keep_full_history: true,
        },
    );
    assert!(
        !plan.should_compact,
        "trailing events already fit under the threshold"
    );

    // The SQLite store supports explicit pruning with a snapshot anchor.
    assert_eq!(
        harness_session_store::prune_plan(&stored, 5).expect("anchored"),
        6
    );
    let removed = store.prune_events_before(session, 4).await.expect("prune");
    assert_eq!(removed, 4);

    // Trailing replay over the surviving stream stays valid.
    let after = store.load_session(session).await.expect("load");
    assert_eq!(
        after.events.len(),
        0,
        "the surviving event is covered by the sequence-5 snapshot"
    );
    validate_trailing_replay(&after, GapPolicy::Strict).expect("replay stays valid");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Diagnostics scan both stores read-only and report gaps/duplicates.
#[tokio::test]
async fn diagnostics_scan_works_across_implementations() {
    for store in impls() {
        let session = SessionId::new();
        store
            .append(harness_session_store::DurableSessionEvent {
                session_sequence: Some(1),
                envelope: state_changed(session, 1, AgentStatus::PreparingContext),
            })
            .await
            .expect("append");
        store
            .save_snapshot(harness_session_store::testing::test_snapshot(session, 1))
            .await
            .expect("snapshot");

        let diagnostics = diagnose_store(&*store).await;
        let report = diagnostics
            .sessions
            .iter()
            .find(|report| report.session_id == session)
            .expect("session diagnosed");
        assert!(report.has_snapshot);
        assert_eq!(report.durable_event_count, 1);
        assert!(report.is_healthy());
    }
}

/// Returns one JSONL and one SQLite store instance for cross-implementation
/// checks (fresh scratch paths per call).
fn impls() -> Vec<Arc<dyn SessionStore>> {
    let tag = format!(
        "rc300-impls-{}-{}",
        std::process::id(),
        Timestamp::now().timestamp_millis()
    );
    let dir = std::env::temp_dir().join(&tag);
    let jsonl: Arc<dyn SessionStore> = Arc::new(JsonlSessionStore::new(dir.join("jsonl")));
    let sqlite: Arc<dyn SessionStore> =
        Arc::new(SqliteSessionStore::open(dir.join("store.db")).expect("open sqlite"));
    vec![jsonl, sqlite]
}
