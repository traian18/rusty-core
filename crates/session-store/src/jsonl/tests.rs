use std::path::PathBuf;

use tokio::io::AsyncWriteExt;

use super::*;
use crate::store::{DurableSessionEvent, SessionStore, StoreError};
use rusty_protocol::ids::SessionId;
use rusty_protocol::{
    commands::AgentStatus,
    events::{AgentEvent, AgentEventEnvelope, EventVisibility},
    ids::{AgentId, EventId, RunId, Timestamp},
};

fn temp_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "harness-jsonl-test-{}-{}",
        std::process::id(),
        EventId::new()
    ))
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

#[tokio::test]
async fn appends_and_loads_a_session() {
    let directory = temp_dir();
    let store = JsonlSessionStore::with_sync_interval(&directory, 1);
    let session = SessionId::new();
    store.append(event(session, 1)).await.expect("append");
    let stored = store.load_session(session).await.expect("load");
    assert_eq!(stored.events.len(), 1);
    assert!(stored.snapshot.is_none());
    let _ = tokio::fs::remove_dir_all(directory).await;
}

#[tokio::test]
async fn missing_session_is_reported() {
    let store = JsonlSessionStore::new(temp_dir());
    let error = store
        .load_session(SessionId::new())
        .await
        .expect_err("missing");
    assert!(matches!(error, StoreError::NotFound(_)));
}

#[tokio::test]
async fn current_sequence_is_zero_for_fresh_session() {
    let store = JsonlSessionStore::new(temp_dir());
    assert_eq!(
        store
            .current_sequence(SessionId::new())
            .await
            .expect("sequence"),
        0
    );
}

#[tokio::test]
async fn current_sequence_resumes_from_snapshot_and_events() {
    let directory = temp_dir();
    let store = JsonlSessionStore::with_sync_interval(&directory, 1);
    let session = SessionId::new();
    store.append(event(session, 1)).await.expect("append");
    store.append(event(session, 2)).await.expect("append");
    assert_eq!(store.current_sequence(session).await.expect("sequence"), 2);
    let _ = tokio::fs::remove_dir_all(directory).await;
}

#[tokio::test]
async fn truncated_trailing_line_does_not_corrupt_session() {
    let directory = temp_dir();
    let store = JsonlSessionStore::with_sync_interval(&directory, 1);
    let session = SessionId::new();
    store.append(event(session, 1)).await.expect("append");
    let path = store.path_for(session);
    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .await
        .expect("open");
    file.write_all(b"{\"kind\":\"event\"")
        .await
        .expect("truncate");
    drop(file);
    let stored = store.load_session(session).await.expect("load");
    assert_eq!(stored.events.len(), 1);
    let _ = tokio::fs::remove_dir_all(directory).await;
}
