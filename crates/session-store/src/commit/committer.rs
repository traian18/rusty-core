//! Sequencing, persisting and checkpoint requests for one session.

use std::sync::{Arc, Mutex};

use rusty_protocol::ids::SessionId;
use tokio::sync::Mutex as AsyncMutex;

use super::{
    CheckpointReason, CheckpointRequester, CommitError, CommittedEvent, DurabilityPolicy,
    SessionSequencer,
};
use crate::store::{is_durable, DurableSessionEvent, SessionStore, StoreError};

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
        mut envelope: rusty_protocol::events::AgentEventEnvelope,
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
            let append_start = std::time::Instant::now();
            let append_result = self.store.append(durable).await;
            metrics::histogram!("harness_store_commit_duration_seconds")
                .record(append_start.elapsed().as_secs_f64());
            match append_result {
                Ok(()) => {
                    metrics::counter!("harness_store_commits_total", "outcome" => "success")
                        .increment(1);
                    self.note_durable_append(sequence)
                }
                Err(StoreError::InvalidState(message))
                    if message.contains("UNIQUE") || message.contains("unique") =>
                {
                    metrics::counter!("harness_store_commits_total", "outcome" => "duplicate_sequence").increment(1);
                    return Err(CommitError::DuplicateSequence(sequence));
                }
                Err(error) => match self.policy {
                    DurabilityPolicy::Strict => {
                        metrics::counter!("harness_store_commits_total", "outcome" => "failed_strict").increment(1);
                        return Err(CommitError::Store(error));
                    }
                    DurabilityPolicy::BestEffort => {
                        metrics::counter!("harness_store_commits_total", "outcome" => "degraded")
                            .increment(1);
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
