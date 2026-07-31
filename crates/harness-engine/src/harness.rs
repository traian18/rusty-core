//! Top-level public harness entry point.

use std::sync::Arc;

use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::SessionId;
use harness_runtime::scheduler::Scheduler;
use harness_runtime::session_manager::SessionManager;
use harness_runtime::traits::{EventSink, SimpleToolRegistry};
use harness_runtime::workspace::FakeWorkspace;
use harness_runtime::{IntegrationError, IntegrationFactory, IntegrationRegistry};
use harness_session_store::SessionStore;

use crate::builder::NoopSessionStore;
use crate::session_builder::{HarnessError, SessionBuilder, SessionHandle};

/// Public entry point for registering integrations and creating sessions.
pub struct Harness {
    pub(crate) integrations: Arc<IntegrationRegistry>,
    pub(crate) sessions: Arc<SessionManager>,
    pub(crate) session_store: Arc<dyn SessionStore>,
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
        SessionBuilder::with_integrations_and_manager(
            self.integrations.clone(),
            self.sessions.clone(),
        )
    }

    /// Restore a previously persisted session, returning a live
    /// [`SessionHandle`].
    ///
    /// The harness's integration registry re-creates the stored agents'
    /// execution backends, and the configured [`SessionStore`] supplies the
    /// durable snapshot/event history. Session-scoped dependencies the
    /// harness does not own (tool registry, workspace, external event sink)
    /// are populated with empty/in-memory defaults — the restored session's
    /// own state (agents, messages, capabilities, usage, backend bindings)
    /// comes entirely from the store.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::SessionManager`] when no store is configured,
    /// the session is not found, the stored session has no snapshot
    /// checkpoint, or a stored backend cannot be re-created.
    pub async fn restore_session(&self, id: SessionId) -> Result<SessionHandle, HarnessError> {
        let runtime = self
            .sessions
            .restore_session(
                id,
                self.integrations.clone(),
                Arc::new(SimpleToolRegistry::new()),
                Arc::new(FakeWorkspace::new()),
                Arc::new(NoopEventSink),
            )
            .await?;
        Ok(SessionHandle::from_runtime(runtime))
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
