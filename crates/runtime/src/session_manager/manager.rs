//! Concrete session manager implementation coordinating registry, lifecycle, and restore.

use std::sync::Arc;

use rusty_session_store::{RestorePolicy, SessionStore};

use crate::scheduler::{Scheduler, SchedulerConfig};

use crate::session_manager::registry::ActiveSessionRegistry;

/// Central session coordinator managing multi-session lifecycle, durable restoration,
/// and runtime capacity.
///
/// Implements the [`SessionRegistry`](super::SessionRegistry), [`SessionLifecycle`](super::SessionLifecycle), and [`SessionRestorer`](super::SessionRestorer) contracts
/// while encapsulating underlying sub-systems (storage, registry, scheduling, supervision).
pub struct SessionManager {
    pub(super) registry: ActiveSessionRegistry,
    pub(super) scheduler: Arc<Scheduler>,
    pub(super) store: Option<Arc<dyn SessionStore>>,
    pub(super) restore_policy: RestorePolicy,
}

impl SessionManager {
    /// Creates a new `SessionManager` with the provided scheduler and no session store.
    pub fn new(scheduler: Arc<Scheduler>) -> Self {
        Self::new_with_store(scheduler, None)
    }

    /// Creates a new `SessionManager` with a scheduler and an optional persistent session store.
    pub fn new_with_store(scheduler: Arc<Scheduler>, store: Option<Arc<dyn SessionStore>>) -> Self {
        Self {
            registry: ActiveSessionRegistry::new(),
            scheduler,
            store,
            restore_policy: RestorePolicy::RejectMissing,
        }
    }

    /// Builder method to configure a persistent session store.
    pub fn with_session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Builder method to configure the host dependency restore policy.
    pub fn with_restore_policy(mut self, policy: RestorePolicy) -> Self {
        self.restore_policy = policy;
        self
    }

    /// Returns the shared scheduler used for permit admission and throttling.
    pub fn scheduler(&self) -> Arc<Scheduler> {
        self.scheduler.clone()
    }

    /// Returns a reference to the internal active session registry.
    pub fn registry(&self) -> &ActiveSessionRegistry {
        &self.registry
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new(Arc::new(Scheduler::new(SchedulerConfig::default())))
    }
}
