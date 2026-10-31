//! [`SessionStore`] for [`MemoryStore`].

use async_trait::async_trait;
use rusty_protocol::ids::SessionId;

use super::MemoryStore;
use crate::store::{
    DurableSessionEvent, DurableSessionSnapshot, RawRecord, SessionStore, StoreError, StoredSession,
};

#[async_trait]
impl SessionStore for MemoryStore {
    async fn load_session(&self, id: SessionId) -> Result<StoredSession, StoreError> {
        let events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .cloned()
            .unwrap_or_default();
        let snapshot = self
            .snapshots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .cloned();
        if snapshot.is_none() && events.is_empty() {
            return Err(StoreError::NotFound(id));
        }
        let mut events = events;
        if let Some(snapshot) = &snapshot {
            events.retain(|event| {
                event
                    .session_sequence
                    .is_some_and(|sequence| sequence > snapshot.session_sequence)
            });
        }
        Ok(StoredSession {
            session_id: id,
            snapshot,
            events,
        })
    }

    async fn current_sequence(&self, id: SessionId) -> Result<u64, StoreError> {
        let events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .cloned()
            .unwrap_or_default();
        let snapshot_max = self
            .snapshots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .map(|snapshot| snapshot.session_sequence)
            .unwrap_or(0);
        let event_max = events
            .iter()
            .filter_map(|event| event.session_sequence)
            .max()
            .unwrap_or(0);
        Ok(snapshot_max.max(event_max))
    }

    /// Reads the durable history after `since_seq` directly from the raw
    /// event log, without applying the snapshot cutoff `load_session` uses.
    ///
    /// This must NOT delegate to the default trait-provided implementation
    /// (which is built on `load_session`): a terminal run's checkpoint can
    /// fold events at-or-below the snapshot boundary out of `load_session`'s
    /// trailing view, which is correct for `load_session` but wrong here —
    /// a reconnecting subscriber asking "everything since sequence N" must
    /// see those events regardless of snapshot compaction. Both real
    /// backends (`JsonlSessionStore`, `SqliteSessionStore`) already query
    /// their raw event log directly for the same reason; this brings the
    /// in-memory test double's behavior in line with them.
    async fn events_since(
        &self,
        id: SessionId,
        since_seq: u64,
    ) -> Result<Vec<DurableSessionEvent>, StoreError> {
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .cloned()
            .unwrap_or_default();
        events.retain(|event| event.session_sequence.is_some_and(|seq| seq > since_seq));
        Ok(events)
    }

    async fn raw_records(&self, id: SessionId) -> Result<Vec<RawRecord>, StoreError> {
        let mut records = Vec::new();
        let events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .cloned()
            .unwrap_or_default();
        records.extend(events.into_iter().map(RawRecord::Event));
        let snapshot = self
            .snapshots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .cloned();
        if let Some(snapshot) = snapshot {
            records.push(RawRecord::Snapshot(snapshot));
        }
        Ok(records)
    }

    async fn append(&self, event: DurableSessionEvent) -> Result<(), StoreError> {
        if *self
            .fail_appends
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            return Err(StoreError::Backend("scripted append failure".into()));
        }
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = events.entry(event.envelope.session_id).or_default();
        if let Some(sequence) = event.session_sequence {
            if entry
                .iter()
                .any(|existing| existing.session_sequence == Some(sequence))
            {
                return Err(StoreError::InvalidState(
                    "UNIQUE constraint failed: duplicate session_sequence".into(),
                ));
            }
        }
        entry.push(event);
        Ok(())
    }

    async fn save_snapshot(&self, snapshot: DurableSessionSnapshot) -> Result<(), StoreError> {
        if *self
            .fail_snapshots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            return Err(StoreError::Backend("scripted snapshot failure".into()));
        }
        self.snapshots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(snapshot.session_id, snapshot);
        Ok(())
    }
}
