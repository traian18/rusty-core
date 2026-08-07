//! Public test doubles for the store crate.
//!
//! [`MemoryStore`] is a fully in-memory [`SessionStore`] with the same
//! contract behavior as the real stores (duplicate-sequence rejection,
//! snapshot replacement, snapshot-cutoff reads) plus scripted append
//! failures for exercising durability policies. It is used by this crate's
//! tests and is exported for embedding crates that want deterministic
//! persistence in their own test suites.
//!
//! [`FaultInjectingStore`] is a generic decorator over any `Arc<dyn
//! SessionStore>` that lets a test script per-call, per-method failures
//! (M2 requirement: "persistence failure before and after a transition").
//! Unlike `MemoryStore::set_fail_appends`, it wraps a real backing store
//! (in-memory, JSONL, or SQLite) so crash/restart fixtures can exercise the
//! genuine persistence path and then "restart" against the same underlying
//! data with faults cleared.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_protocol::ids::{SessionId, Timestamp};

use crate::store::{
    DurableSessionEvent, DurableSessionSnapshot, RawRecord, SessionStore, StoreError,
    StoredSession,
};

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

// ---------------------------------------------------------------------------
// FaultInjectingStore
// ---------------------------------------------------------------------------

/// A per-call fault-injection decorator over any `Arc<dyn SessionStore>`.
///
/// Register a hook with [`Self::set_fault_hook`] that receives the operation
/// name (`"load_session"`, `"append"`, `"save_snapshot"`, or
/// `"current_sequence"`) and a 1-based call counter for that operation, and
/// returns `Some(error)` to fail that specific call or `None` to forward it
/// to the wrapped store. This lets a test script exact failure points (e.g.
/// "fail the 3rd append" or "fail every save_snapshot") against a real
/// backing store, then clear the hook to simulate a clean restart that
/// resumes from whatever the backing store actually persisted.
/// A scripted fault hook: given an operation name and a 1-based per-operation
/// call counter, returns `Some(error)` to fail that call or `None` to let it
/// pass through to the wrapped store.
type FaultHook = Arc<dyn Fn(&str, u64) -> Option<StoreError> + Send + Sync>;

pub struct FaultInjectingStore {
    inner: Arc<dyn SessionStore>,
    hook: Mutex<Option<FaultHook>>,
    counters: Mutex<HashMap<String, u64>>,
}

impl FaultInjectingStore {
    /// Wraps `inner`, initially with no injected faults.
    pub fn new(inner: Arc<dyn SessionStore>) -> Self {
        Self {
            inner,
            hook: Mutex::new(None),
            counters: Mutex::new(HashMap::new()),
        }
    }

    /// Installs a fault hook, replacing any previous one.
    pub fn set_fault_hook<F>(&self, hook: F)
    where
        F: Fn(&str, u64) -> Option<StoreError> + Send + Sync + 'static,
    {
        *self
            .hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(hook));
    }

    /// Removes any installed fault hook, restoring normal pass-through
    /// behavior. Simulates a clean restart of the persistence layer while
    /// keeping whatever data the backing store already has.
    pub fn clear_fault_hook(&self) {
        *self
            .hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    /// Resets per-operation call counters without touching the fault hook.
    pub fn reset_counters(&self) {
        self.counters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    fn check(&self, op: &str) -> Option<StoreError> {
        let call_count = {
            let mut counters = self
                .counters
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let count = counters.entry(op.to_string()).or_insert(0);
            *count += 1;
            *count
        };
        let hook = self
            .hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        hook.and_then(|hook| hook(op, call_count))
    }
}

#[async_trait]
impl SessionStore for FaultInjectingStore {
    async fn load_session(&self, id: SessionId) -> Result<StoredSession, StoreError> {
        if let Some(error) = self.check("load_session") {
            return Err(error);
        }
        self.inner.load_session(id).await
    }

    async fn current_sequence(&self, id: SessionId) -> Result<u64, StoreError> {
        if let Some(error) = self.check("current_sequence") {
            return Err(error);
        }
        self.inner.current_sequence(id).await
    }

    async fn raw_records(&self, id: SessionId) -> Result<Vec<RawRecord>, StoreError> {
        if let Some(error) = self.check("raw_records") {
            return Err(error);
        }
        self.inner.raw_records(id).await
    }

    async fn append(&self, event: DurableSessionEvent) -> Result<(), StoreError> {
        if let Some(error) = self.check("append") {
            return Err(error);
        }
        self.inner.append(event).await
    }

    async fn save_snapshot(&self, snapshot: DurableSessionSnapshot) -> Result<(), StoreError> {
        if let Some(error) = self.check("save_snapshot") {
            return Err(error);
        }
        self.inner.save_snapshot(snapshot).await
    }

    async fn list_sessions(&self) -> Result<Vec<crate::store::SessionSummary>, StoreError> {
        if let Some(error) = self.check("list_sessions") {
            return Err(error);
        }
        self.inner.list_sessions().await
    }

    async fn events_since(
        &self,
        id: SessionId,
        since_seq: u64,
    ) -> Result<Vec<DurableSessionEvent>, StoreError> {
        if let Some(error) = self.check("events_since") {
            return Err(error);
        }
        self.inner.events_since(id, since_seq).await
    }

    async fn prune_events_before(
        &self,
        id: SessionId,
        sequence: u64,
    ) -> Result<u64, StoreError> {
        if let Some(error) = self.check("prune_events_before") {
            return Err(error);
        }
        self.inner.prune_events_before(id, sequence).await
    }
}

#[cfg(test)]
mod fault_injection_tests {
    use super::*;
    use crate::commit::{DurabilityPolicy, SessionCommitter};
    use harness_protocol::commands::AgentStatus;
    use harness_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
    use harness_protocol::ids::{AgentId, EventId, RunId};

    fn durable_event(session_id: SessionId) -> AgentEventEnvelope {
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
            event: AgentEvent::StateChanged {
                from: AgentStatus::Idle,
                to: AgentStatus::PreparingContext,
            },
        }
    }

    /// M2 crash fixture: a `Strict`-policy commit whose durable append fails
    /// must not advance the durably observable state. A fresh committer
    /// built over the same backing store after the fault clears (simulating
    /// a process restart) must resume at the sequence the store actually
    /// has, without a gap and without silently losing the failed event.
    #[tokio::test]
    async fn strict_policy_failed_append_does_not_leave_a_persisted_gap() {
        let backing = Arc::new(MemoryStore::new());
        let faulty = Arc::new(FaultInjectingStore::new(backing.clone()));
        let session = SessionId::new();

        faulty.set_fault_hook(|op, call_count| {
            if op == "append" && call_count == 1 {
                Some(StoreError::Backend("simulated crash mid-write".into()))
            } else {
                None
            }
        });

        let committer = SessionCommitter::new(faulty.clone(), session)
            .with_durability_policy(DurabilityPolicy::Strict);
        let result = committer.commit(durable_event(session)).await;
        assert!(
            result.is_err(),
            "strict policy must surface the store failure rather than publish"
        );

        // Nothing was actually persisted by the failed attempt.
        assert_eq!(backing.current_sequence(session).await.unwrap_or(0), 0);

        // Simulate a restart: clear the fault and build a fresh committer
        // over the same backing store.
        faulty.clear_fault_hook();
        let restarted = SessionCommitter::new(faulty, session)
            .with_durability_policy(DurabilityPolicy::Strict);
        let committed = restarted
            .commit(durable_event(session))
            .await
            .expect("commit")
            .expect("published");

        // The sequencer must resume from the store's true durable state
        // (sequence 0), so the retried event lands at sequence 1 with no
        // gap and no duplicate.
        assert_eq!(committed.envelope.session_sequence, Some(1));
        let stored = backing.load_session(session).await.expect("load");
        assert_eq!(stored.events.len(), 1);
    }

    /// M2: `BestEffort` policy must let execution continue but must mark
    /// the event `degraded`, and the degraded event must not silently
    /// appear as durably persisted.
    #[tokio::test]
    async fn best_effort_policy_marks_degraded_and_does_not_fabricate_durability() {
        let backing = Arc::new(MemoryStore::new());
        let faulty = Arc::new(FaultInjectingStore::new(backing.clone()));
        let session = SessionId::new();

        faulty.set_fault_hook(|op, _| {
            if op == "append" {
                Some(StoreError::Backend("simulated disk full".into()))
            } else {
                None
            }
        });

        let committer = SessionCommitter::new(faulty, session)
            .with_durability_policy(DurabilityPolicy::BestEffort);
        let committed = committer
            .commit(durable_event(session))
            .await
            .expect("commit does not error under BestEffort")
            .expect("still published");

        assert!(committed.degraded, "a failed append must be marked degraded");
        assert_eq!(
            backing.current_sequence(session).await.unwrap_or(0),
            0,
            "a degraded event must not appear durably persisted in the backing store"
        );
    }

    /// M2: a `save_snapshot` failure must not corrupt or lose prior durable
    /// events; the store's event history remains queryable independent of
    /// checkpoint failures.
    #[tokio::test]
    async fn snapshot_failure_does_not_lose_durable_events() {
        let backing = Arc::new(MemoryStore::new());
        let faulty = Arc::new(FaultInjectingStore::new(backing.clone()));
        let session = SessionId::new();

        let committer = SessionCommitter::new(faulty.clone(), session);
        committer
            .commit(durable_event(session))
            .await
            .expect("commit")
            .expect("published");

        faulty.set_fault_hook(|op, _| {
            if op == "save_snapshot" {
                Some(StoreError::Backend("simulated checkpoint failure".into()))
            } else {
                None
            }
        });

        let snapshot = test_snapshot(session, 1);
        let result = faulty.save_snapshot(snapshot).await;
        assert!(result.is_err());

        let stored = backing.load_session(session).await.expect("load");
        assert_eq!(stored.events.len(), 1, "prior durable events must survive a checkpoint failure");
        assert!(stored.snapshot.is_none(), "the failed snapshot must not appear saved");
    }
}
