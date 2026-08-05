//! Fluent construction chain for the [`Harness`] public API.
//!
//! [`HarnessBuilder`] follows the codebase's "concrete struct, no trait
//! abstraction" style (`SessionManager`, `IntegrationRegistry`,
//! `SessionBuilder` are all concrete too): it collects integration factories
//! and the optional durable [`SessionStore`], then [`build`](HarnessBuilder::build)s
//! a fully wired [`Harness`].
//!
//! ```no_run
//! # use harness_engine::{Harness, HarnessError};
//! # async fn example() -> Result<(), HarnessError> {
//! let harness = Harness::builder()
//!     // .register_integration(anthropic_factory)
//!     // .session_store(sqlite_store)
//!     .build()
//!     .await?;
//! # let _ = harness;
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use harness_protocol::ids::SessionId;
use harness_runtime::scheduler::Scheduler;
use harness_runtime::session_manager::SessionManager;
use harness_runtime::{IntegrationFactory, IntegrationRegistry};
use harness_session_store::{
    DurableSessionEvent, DurableSessionSnapshot, SessionStore, StoreError, StoredSession,
};

use crate::harness::Harness;
use crate::session_builder::HarnessError;

/// Fluent builder for constructing a [`Harness`].
///
/// Collects integration factories and the optional durable session store,
/// then [`build`](Self::build)s the fully wired [`Harness`]. Persistence is
/// optional: when no [`SessionStore`] is configured the builder falls back
/// to an in-memory no-op store (emitting a `tracing::warn!`), so earlier
/// phases' behavior keeps working unchanged while a real deployment gets a
/// visible signal that persistence is not configured.
pub struct HarnessBuilder {
    /// Integration factories to register on the built harness.
    integrations: Vec<Arc<dyn IntegrationFactory>>,
    /// Durable session store; `None` falls back to the in-memory no-op store.
    session_store: Option<Arc<dyn SessionStore>>,
}

impl HarnessBuilder {
    /// Starts an empty builder: no integrations and no session store.
    pub fn new() -> Self {
        Self {
            integrations: Vec::new(),
            session_store: None,
        }
    }

    /// Registers a dynamically constructible integration family.
    ///
    /// The factory is registered under its stable
    /// [`IntegrationFactory::id`] on the built harness, so sessions can
    /// later resolve backends through `.integration(name, config)`.
    pub fn register_integration(mut self, factory: Arc<dyn IntegrationFactory>) -> Self {
        self.integrations.push(factory);
        self
    }

    /// Configures the durable session store used for persistence and restore.
    ///
    /// When omitted, [`build`](Self::build) falls back to an in-memory no-op
    /// store and emits a `tracing::warn!`.
    pub fn session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.session_store = Some(store);
        self
    }

    /// Builds the fully wired [`Harness`].
    ///
    /// Registers every collected integration factory, threads the session
    /// store into the harness's [`SessionManager`] (defaulting to the
    /// in-memory no-op store with a warning when none was configured), and
    /// assembles the [`Harness`].
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::Integration`] if an integration factory
    /// cannot be registered (e.g. the registry lock is poisoned).
    pub async fn build(self) -> Result<Harness, HarnessError> {
        let store: Arc<dyn SessionStore> = match self.session_store {
            Some(store) => store,
            None => {
                tracing::warn!(
                    "no session store configured for Harness::builder(); \
                     using an in-memory no-op store — session persistence \
                     and restore are disabled"
                );
                Arc::new(NoopSessionStore)
            }
        };

        let integrations = Arc::new(IntegrationRegistry::new());
        for factory in self.integrations {
            integrations.register(factory)?;
        }

        let sessions = Arc::new(SessionManager::new_with_store(
            Arc::new(Scheduler::new(Default::default())),
            Some(store.clone()),
        ));

        Ok(Harness {
            integrations,
            sessions,
            session_store: store,
            model_cache: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        })
    }
}

impl Default for HarnessBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl Harness {
    /// Begins the fluent construction chain for a [`Harness`].
    ///
    /// Example (spec §63):
    ///
    /// ```no_run
    /// # use harness_engine::{Harness, HarnessError};
    /// # async fn example() -> Result<(), HarnessError> {
    /// let harness = Harness::builder()
    ///     // .register_integration(anthropic_factory)
    ///     // .session_store(sqlite_store)
    ///     .build()
    ///     .await?;
    /// # let _ = harness;
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder() -> HarnessBuilder {
        HarnessBuilder::new()
    }
}

/// In-memory [`SessionStore`] used when no durable store is configured.
///
/// Every write is accepted and dropped (nothing is persisted), and any
/// requested session reports [`StoreError::NotFound`], so restore through it
/// fails cleanly rather than panicking. This fallback keeps earlier-phase
/// behavior unchanged; `Harness::builder()` warns when it is in use, and
/// `Harness::new()` uses it as the default store.
pub(crate) struct NoopSessionStore;

#[async_trait]
impl SessionStore for NoopSessionStore {
    async fn load_session(&self, id: SessionId) -> Result<StoredSession, StoreError> {
        Err(StoreError::NotFound(id))
    }

    async fn append(&self, _event: DurableSessionEvent) -> Result<(), StoreError> {
        Ok(())
    }

    async fn save_snapshot(&self, _snapshot: DurableSessionSnapshot) -> Result<(), StoreError> {
        Ok(())
    }
}
