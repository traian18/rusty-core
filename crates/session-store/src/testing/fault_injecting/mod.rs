//! A decorator that fails scripted calls of another store.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use crate::store::{SessionStore, StoreError};

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

mod session_store;
