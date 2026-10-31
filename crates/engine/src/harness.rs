//! Top-level public harness entry point.

mod provider_ops;
mod restore;

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use rusty_protocol::events::AgentEventEnvelope;

use rusty_runtime::scheduler::Scheduler;
use rusty_runtime::session_manager::SessionManager;
use rusty_runtime::traits::EventSink;

use rusty_runtime::{IntegrationError, IntegrationFactory, IntegrationRegistry};
use rusty_session_store::{SessionStore, SessionSummary};

use crate::builder::NoopSessionStore;
use crate::providers::{ModelDescriptor, ProviderKey};
use crate::session_builder::{HarnessError, SessionBuilder};

/// Public entry point for registering integrations and creating sessions.
pub struct Harness {
    pub(crate) integrations: Arc<IntegrationRegistry>,
    pub(crate) sessions: Arc<SessionManager>,
    pub(crate) session_store: Arc<dyn SessionStore>,
    pub(crate) model_cache: Arc<RwLock<HashMap<ProviderKey, Vec<ModelDescriptor>>>>,
    pub(crate) orchestration: Option<crate::orchestration::OrchestrationConfig>,
    pub(crate) profiles: crate::profiles::ProfilesConfig,
}

impl Harness {
    /// Create a harness with an empty integration registry, a fresh
    /// [`SessionManager`], and the in-memory no-op [`SessionStore`].
    ///
    /// This mirrors the composition produced by the default
    /// [`HarnessBuilder`](crate::HarnessBuilder): no integrations, no durable
    /// persistence, and session restore routed through the no-op store (which
    /// reports every session as not found). Prefer
    /// [`Harness::builder()`](Self::builder) — and configure a real
    /// [`SessionStore`] via `.session_store(...)` — when persistence and
    /// restore are required.
    pub fn new() -> Self {
        let store: Arc<dyn SessionStore> = Arc::new(NoopSessionStore);
        Self {
            integrations: Arc::new(IntegrationRegistry::new()),
            sessions: Arc::new(SessionManager::new_with_store(
                Arc::new(Scheduler::new(Default::default())),
                Some(store.clone()),
            )),
            session_store: store,
            model_cache: Arc::new(RwLock::new(HashMap::new())),
            orchestration: None,
            profiles: Default::default(),
        }
    }

    /// Register a dynamically constructible integration family.
    pub fn register_integration(
        &self,
        factory: Arc<dyn IntegrationFactory>,
    ) -> Result<(), IntegrationError> {
        self.integrations.register(factory)
    }

    /// Return a handle to the shared [`SessionManager`].
    ///
    /// The returned `Arc` can be used to query active sessions, close
    /// sessions, or restore persisted sessions without going through a
    /// [`SessionBuilder`].
    pub fn session_manager(&self) -> Arc<SessionManager> {
        self.sessions.clone()
    }

    /// Return the harness's shared [`SessionStore`].
    ///
    /// Always populated: the in-memory no-op store when no durable store was
    /// configured (see [`Harness::new`] and
    /// [`HarnessBuilder`](crate::HarnessBuilder)), or the store configured
    /// via `.session_store(...)`.
    pub fn session_store(&self) -> Arc<dyn SessionStore> {
        self.session_store.clone()
    }

    /// Begin building a session using this harness's integration registry
    /// and session manager.
    pub fn session(&self) -> SessionBuilder {
        let builder = SessionBuilder::with_integrations_and_manager(
            self.integrations.clone(),
            self.sessions.clone(),
        )
        .profiles(self.profiles.clone());
        match &self.orchestration {
            Some(config) => builder.orchestration(config.clone()),
            None => builder,
        }
    }

    /// The orchestration configuration, when enabled via
    /// [`HarnessBuilder::orchestration`](crate::HarnessBuilder::orchestration).
    /// Use it to register additional workflow definitions.
    pub fn orchestration(&self) -> Option<&crate::orchestration::OrchestrationConfig> {
        self.orchestration.as_ref()
    }

    /// The host's behavior-profile registry. Register profiles here (e.g.
    /// from an editor); sessions started afterwards can reference them.
    pub fn profiles(&self) -> &crate::profiles::ProfilesConfig {
        &self.profiles
    }

    /// Lists durable sessions newest-first for frontend discovery.
    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, HarnessError> {
        Ok(self.session_store.list_sessions().await?)
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

/// No-op [`EventSink`] used when restoring a session through the harness.
///
/// The restored runtime still publishes events to its session-internal event
/// bus (observable via [`SessionHandle::subscribe`]); this sink only covers
/// the external forwarding path, which the harness does not configure.
struct NoopEventSink;

impl EventSink for NoopEventSink {
    fn send(&self, _envelope: AgentEventEnvelope) {}
}
