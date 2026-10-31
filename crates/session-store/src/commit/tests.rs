use std::sync::Arc;

use super::*;
use crate::store::SessionStore;
use crate::testing::MemoryStore;
use rusty_protocol::commands::AgentStatus;
use rusty_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
use rusty_protocol::ids::SessionId;
use rusty_protocol::ids::{AgentId, EventId, RunId, Timestamp};

fn event(session_id: SessionId, durable: bool) -> AgentEventEnvelope {
    let event = if durable {
        AgentEvent::StateChanged {
            from: AgentStatus::Idle,
            to: AgentStatus::PreparingContext,
        }
    } else {
        AgentEvent::AssistantTextDelta {
            message_id: rusty_protocol::ids::MessageId::new(),
            delta: "partial".into(),
        }
    };
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
        event,
    }
}

#[tokio::test]
async fn commit_assigns_final_sequence_and_persists() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new();
    let committer = SessionCommitter::new(store.clone(), session);
    let committed = committer
        .commit(event(session, true))
        .await
        .expect("commit")
        .expect("published");

    assert_eq!(committed.envelope.session_sequence, Some(1));
    assert!(!committed.degraded);
    let stored = store.load_session(session).await.expect("load");
    assert_eq!(stored.events.len(), 1);
    assert_eq!(stored.events[0].session_sequence, Some(1));
}

#[tokio::test]
async fn ephemeral_events_are_sequenced_but_not_persisted() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new();
    let committer = SessionCommitter::new(store.clone(), session);
    let ephemeral = committer
        .commit(event(session, false))
        .await
        .expect("commit")
        .expect("published");
    let durable = committer
        .commit(event(session, true))
        .await
        .expect("commit")
        .expect("published");

    assert_eq!(ephemeral.envelope.session_sequence, Some(1));
    assert_eq!(durable.envelope.session_sequence, Some(2));
    let stored = store.load_session(session).await.expect("load");
    assert_eq!(stored.events.len(), 1);
    assert_eq!(stored.events[0].session_sequence, Some(2));
}

#[tokio::test]
async fn sequences_are_monotonic_and_never_duplicated() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new();
    let committer = SessionCommitter::new(store, session);
    let mut sequences = Vec::new();
    for _ in 0..64 {
        let sequence = committer
            .commit(event(session, true))
            .await
            .expect("commit")
            .expect("published")
            .envelope
            .session_sequence
            .expect("sequenced");
        sequences.push(sequence);
    }
    assert_eq!(sequences, (1..=64).collect::<Vec<_>>());
}

#[tokio::test]
async fn concurrent_commits_persist_in_authoritative_order() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new();
    let committer = Arc::new(SessionCommitter::new(store.clone(), session));
    let mut tasks = Vec::new();
    for _ in 0..32 {
        let committer = committer.clone();
        tasks.push(tokio::spawn(async move {
            committer
                .commit(event(session, true))
                .await
                .expect("commit")
        }));
    }
    for task in tasks {
        task.await.expect("task").expect("published");
    }
    let stored = store.load_session(session).await.expect("load");
    let sequences = stored
        .events
        .iter()
        .map(|event| event.session_sequence.expect("sequence"))
        .collect::<Vec<_>>();
    assert_eq!(sequences, (1..=32).collect::<Vec<_>>());
}
