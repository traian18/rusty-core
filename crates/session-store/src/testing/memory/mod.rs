//! An in-memory [`SessionStore`].

use std::{collections::HashMap, sync::Mutex};

use rusty_protocol::ids::{SessionId, Timestamp};

use crate::store::{DurableSessionEvent, DurableSessionSnapshot};

/// A fully in-memory [`SessionStore`] with configurable append failures.
pub struct MemoryStore {
    fail_appends: Mutex<bool>,
    fail_snapshots: Mutex<bool>,
    events: Mutex<HashMap<SessionId, Vec<DurableSessionEvent>>>,
    snapshots: Mutex<HashMap<SessionId, DurableSessionSnapshot>>,
}

impl MemoryStore {
    /// Creates an empty in-memory store.
    pub fn new() -> Self {
        Self {
            fail_appends: Mutex::new(false),
            fail_snapshots: Mutex::new(false),
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

    /// Makes every subsequent `save_snapshot` fail with a backend error,
    /// without affecting `append`. Used to script checkpoint-only failures
    /// (M2: "persistence failure before and after a transition").
    pub fn set_fail_snapshots(&self, fail: bool) {
        *self
            .fail_snapshots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = fail;
    }

    /// Whether snapshot saves currently fail.
    pub fn snapshots_failing(&self) -> bool {
        *self
            .fail_snapshots
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
        root_agent_id: rusty_protocol::ids::AgentId::new(),
        agents: Vec::new(),
        session_sequence: sequence,
        timestamp: Timestamp::now(),
        schema_version: crate::version::SCHEMA_VERSION,
        metadata: crate::store::DurableSessionMetadata::default(),
    }
}

// ---------------------------------------------------------------------------
// FaultInjectingStore
// ---------------------------------------------------------------------------

mod session_store;
