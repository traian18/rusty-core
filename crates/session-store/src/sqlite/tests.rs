use super::errors::now_ms;
use super::SqliteSessionStore;
use crate::store::{DurableSessionEvent, SessionStore};
use rusty_protocol::commands::AgentStatus;
use rusty_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
use rusty_protocol::ids::{AgentId, EventId, RunId};
use rusty_protocol::ids::{SessionId, Timestamp};

fn temp_db(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "harness-sqlite-{tag}-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir.join("store.db")
}

fn event(session_id: SessionId, sequence: u64) -> DurableSessionEvent {
    DurableSessionEvent {
        envelope: AgentEventEnvelope {
            event_id: EventId::new(),
            session_id,
            agent_id: AgentId::new(),
            parent_agent_id: None,
            run_id: Some(RunId::new()),
            agent_sequence: sequence,
            session_sequence: Some(sequence),
            timestamp: Timestamp::now(),
            visibility: EventVisibility::User,
            event: AgentEvent::StateChanged {
                from: AgentStatus::Idle,
                to: AgentStatus::PreparingContext,
            },
        },
        session_sequence: Some(sequence),
    }
}

#[test]
fn opens_a_wal_database() {
    let path = temp_db("wal");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let store = runtime
        .block_on(async { SqliteSessionStore::open(&path) })
        .expect("open store");
    drop(store);
    let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
}

#[tokio::test]
async fn current_sequence_is_zero_for_fresh_session() {
    let store = SqliteSessionStore::open(temp_db("seq-zero")).expect("open store");
    assert_eq!(
        store
            .current_sequence(SessionId::new())
            .await
            .expect("sequence"),
        0
    );
}

#[tokio::test]
async fn current_sequence_tracks_committed_events() {
    let store = SqliteSessionStore::open(temp_db("seq")).expect("open store");
    let session = SessionId::new();
    store.append(event(session, 1)).await.expect("append 1");
    store.append(event(session, 2)).await.expect("append 2");
    assert_eq!(store.current_sequence(session).await.expect("sequence"), 2);
}

#[tokio::test]
async fn prune_removes_only_events_at_or_below_sequence() {
    let store = SqliteSessionStore::open(temp_db("prune")).expect("open store");
    let session = SessionId::new();
    store.append(event(session, 1)).await.expect("append 1");
    store.append(event(session, 2)).await.expect("append 2");
    store.append(event(session, 3)).await.expect("append 3");

    let removed = store.prune_events_before(session, 2).await.expect("prune");
    assert_eq!(removed, 2);

    let stored = store.load_session(session).await.expect("load");
    let sequences: Vec<u64> = stored
        .events
        .iter()
        .filter_map(|event| event.session_sequence)
        .collect();
    assert_eq!(
        sequences,
        vec![3],
        "only events above the prune point survive"
    );
}

#[tokio::test]
async fn snapshot_version_and_metadata_round_trip() {
    let store = SqliteSessionStore::open(temp_db("snap-meta")).expect("open store");
    let session = SessionId::new();
    let mut snapshot = crate::testing::test_snapshot(session, 1);
    snapshot.metadata.workspace_identity = Some("/srv/prod".into());
    snapshot.metadata.integration_references = vec!["anthropic".into()];
    store.save_snapshot(snapshot).await.expect("save snapshot");

    let stored = store.load_session(session).await.expect("load");
    let loaded = stored.snapshot.expect("snapshot");
    assert_eq!(loaded.schema_version, crate::version::SCHEMA_VERSION);
    assert_eq!(
        loaded.metadata.workspace_identity.as_deref(),
        Some("/srv/prod")
    );
    assert_eq!(loaded.metadata.integration_references, vec!["anthropic"]);
}
