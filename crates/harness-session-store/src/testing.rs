//! Public test doubles for the store crate.
//!
//! [`MemoryStore`] is a fully in-memory [`SessionStore`] with the same
//! contract behavior as the real stores (duplicate-sequence rejection,
//! snapshot replacement, snapshot-cutoff reads) plus scripted append
//! failures for exercising durability policies. It is used by this crate's
//! tests and is exported for embedding crates that want deterministic
//! persistence in their own test suites.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use harness_protocol::ids::{SessionId, Timestamp};

use crate::store::{
    DurableSessionEvent, DurableSessionSnapshot, RawRecord, SessionStore, StoreError,
    StoredSession,
};

/// A fully in-memory [`SessionStore`] with configurable append failures.
pub struct MemoryStore {
    fail_appends: Mutex<bool>,
    events: Mutex<HashMap<SessionId, Vec<DurableSessionEvent>>>,
    snapshots: Mutex<HashMap<SessionId, DurableSessionSnapshot>>,
}

impl MemoryStore {
    /// Creates an empty in-memory store.
    pub fn new() -> Self {
        Self {
            fail_appends: Mutex::new(false),
            events: Mutex::new(HashMap::new()),
            snapshots: Mutex::new(HashMap::new()),
        }
    }

    /// Makes every subsequent append fail with a backend error.
    pub fn set_fail_appends(&self, fail: bool) {
        *self
            .fail_appends
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = fail;
    }

    /// Whether appends currently fail.
    pub fn appends_failing(&self) -> bool {
        *self
            .fail_appends
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Builds a minimal [`DurableSessionSnapshot`] for tests.
pub fn test_snapshot(session_id: SessionId, sequence: u64) -> DurableSessionSnapshot {
    DurableSessionSnapshot {
        session_id,
        root_agent_id: harness_protocol::ids::AgentId::new(),
        agents: Vec::new(),
        session_sequence: sequence,
        timestamp: Timestamp::now(),
        schema_version: crate::version::SCHEMA_VERSION,
        metadata: crate::store::DurableSessionMetadata::default(),
    }
}

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
        self.snapshots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(snapshot.session_id, snapshot);
        Ok(())
    }
}
