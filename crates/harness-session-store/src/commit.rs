
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;

use harness_protocol::ids::SessionId;

use crate::store::{is_durable, DurableSessionEvent, SessionStore, StoreError};

#[derive(Debug, Clone, thiserror::Error)]
pub enum CommitError {
    #[error("durable store error: {0}")]
    Store(#[from] StoreError),
    #[error("duplicate session sequence {0}")]
    DuplicateSequence(u64),
    #[error("session store for {0} is closed")]
    Closed(SessionId),
    #[error("invalid commit payload: {0}")]
    InvalidEvent(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DurabilityPolicy {
    #[default]
    Strict,
    BestEffort,
}

#[derive(Debug, Clone)]
pub struct CommittedEvent {
    pub envelope: harness_protocol::events::AgentEventEnvelope,
    pub degraded: bool,
}

pub struct SessionSequencer {
    store: Arc<dyn SessionStore>,
    session_id: SessionId,
    state: Mutex<SequencerState>,
}

enum SequencerState {
    Uninitialized,
    Ready { next: u64 },
}

impl SessionSequencer {
    pub fn new(store: Arc<dyn SessionStore>, session_id: SessionId) -> Self {
        Self {
            store,
            session_id,
            state: Mutex::new(SequencerState::Uninitialized),
        }
    }

    pub async fn next_sequence(&self) -> Result<u64, StoreError> {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match *state {
                SequencerState::Ready { next } => {
                    let current = next;
                    *state = SequencerState::Ready {
                        next: next.saturating_add(1),
                    };
                    return Ok(current);
                }
                SequencerState::Uninitialized => {}
            }
        }

        let committed = self.store.current_sequence(self.session_id).await?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match *state {
            SequencerState::Ready { next } => {
                let current = next;
                *state = SequencerState::Ready {
                    next: next.saturating_add(1),
                };
                Ok(current)
            }
            SequencerState::Uninitialized => {
                let next = committed.saturating_add(1);
                *state = SequencerState::Ready {
                    next: next.saturating_add(1),
                };
                Ok(next)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointReason {
    TerminalRun,
    DurableEventCount,
    Explicit,
}

pub trait CheckpointRequester: Send + Sync {
    fn request_checkpoint(&self, session_id: SessionId, at_sequence: u64, reason: CheckpointReason);
}

pub struct SessionCommitter {
    store: Arc<dyn SessionStore>,
    session_id: SessionId,
    sequencer: SessionSequencer,
    commit_lock: AsyncMutex<()>,
    policy: DurabilityPolicy,
    snapshot_every: u64,
    appends_since_checkpoint: Mutex<u64>,
    checkpoint_requester: Option<Arc<dyn CheckpointRequester>>,
}

pub const DEFAULT_SNAPSHOT_EVERY: u64 = 512;

impl SessionCommitter {
    pub fn new(store: Arc<dyn SessionStore>, session_id: SessionId) -> Self {
        Self {
            store: store.clone(),
            session_id,
            sequencer: SessionSequencer::new(store, session_id),
            commit_lock: AsyncMutex::new(()),
            policy: DurabilityPolicy::Strict,
            snapshot_every: DEFAULT_SNAPSHOT_EVERY,
            appends_since_checkpoint: Mutex::new(0),
            checkpoint_requester: None,
        }
    }

    pub fn with_durability_policy(mut self, policy: DurabilityPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_snapshot_every(mut self, snapshot_every: u64) -> Self {
        self.snapshot_every = snapshot_every;
        self
    }

    pub fn with_checkpoint_requester(mut self, requester: Arc<dyn CheckpointRequester>) -> Self {
        self.checkpoint_requester = Some(requester);
        self
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn store(&self) -> Arc<dyn SessionStore> {
        self.store.clone()
    }

    pub fn sequencer(&self) -> &SessionSequencer {
        &self.sequencer
    }

    pub async fn commit(
        &self,
        mut envelope: harness_protocol::events::AgentEventEnvelope,
    ) -> Result<Option<CommittedEvent>, CommitError> {
        if envelope.event_id.as_uuid().is_nil() {
            return Err(CommitError::InvalidEvent(
                "event carries no stable event_id".into(),
            ));
        }

        let _commit_guard = self.commit_lock.lock().await;

        let sequence = self
            .sequencer
            .next_sequence()
            .await
            .map_err(CommitError::Store)?;
        envelope.session_sequence = Some(sequence);

        if is_durable(&envelope.event) {
            let durable = DurableSessionEvent {
                envelope: envelope.clone(),
                session_sequence: Some(sequence),
            };
            match self.store.append(durable).await {
                Ok(()) => self.note_durable_append(sequence),
                Err(StoreError::InvalidState(message))
                    if message.contains("UNIQUE") || message.contains("unique") =>
                {
                    return Err(CommitError::DuplicateSequence(sequence));
                }
                Err(error) => match self.policy {
                    DurabilityPolicy::Strict => return Err(CommitError::Store(error)),
                    DurabilityPolicy::BestEffort => {
                        tracing::warn!(
                            %sequence,
                            %error,
                            "durable event persisted in degraded mode (BestEffort policy)"
                        );
                        return Ok(Some(CommittedEvent {
                            envelope,
                            degraded: true,
                        }));
                    }
                },
            }
        }

        Ok(Some(CommittedEvent {
            envelope,
            degraded: false,
        }))
    }

    fn note_durable_append(&self, sequence: u64) {
        let mut count = self
            .appends_since_checkpoint
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *count = count.saturating_add(1);
        let due = self.snapshot_every > 0 && *count >= self.snapshot_every;
        if due {
            *count = 0;
            if let Some(requester) = &self.checkpoint_requester {
                requester.request_checkpoint(
                    self.session_id,
                    sequence,
                    CheckpointReason::DurableEventCount,
                );
            }
        }
    }

    pub fn checkpoint_for_terminal_run(&self, sequence: u64) {
        if let Some(requester) = &self.checkpoint_requester {
            requester.request_checkpoint(self.session_id, sequence, CheckpointReason::TerminalRun);
        }
    }
}

pub struct RecordingSink {
    published: Mutex<Vec<harness_protocol::events::AgentEventEnvelope>>,
}

impl RecordingSink {
    pub fn new() -> Self {
        Self {
            published: Mutex::new(Vec::new()),
        }
    }

    pub fn record(&self, envelope: harness_protocol::events::AgentEventEnvelope) {
        self.published
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(envelope);
    }

    pub fn published(&self) -> Vec<harness_protocol::events::AgentEventEnvelope> {
        self.published
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn sequences(&self) -> Vec<u64> {
        self.published()
            .into_iter()
            .filter_map(|envelope| envelope.session_sequence)
            .collect()
    }
}

impl Default for RecordingSink {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MemoryStore;
    use harness_protocol::commands::AgentStatus;
    use harness_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
    use harness_protocol::ids::{AgentId, EventId, RunId, Timestamp};

    fn event(session_id: SessionId, durable: bool) -> AgentEventEnvelope {
        let event = if durable {
            AgentEvent::StateChanged {
                from: AgentStatus::Idle,
                to: AgentStatus::PreparingContext,
            }
        } else {
            AgentEvent::AssistantTextDelta {
                message_id: harness_protocol::ids::MessageId::new(),
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
                committer.commit(event(session, true)).await.expect("commit")
            }));
        }
        for task in tasks {
            task.await.expect("task").expect("published");
        }
        let stored = store.load_session(session).await.expect("load");
        let sequences = stored.events.iter()
            .map(|event| event.session_sequence.expect("sequence"))
            .collect::<Vec<_>>();
        assert_eq!(sequences, (1..=32).collect::<Vec<_>>());
    }
}
