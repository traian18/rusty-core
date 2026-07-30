//! Top-level public harness entry point.

use std::sync::Arc;

use harness_runtime::session_manager::SessionManager;
use harness_runtime::{IntegrationError, IntegrationFactory, IntegrationRegistry};

use crate::session_builder::SessionBuilder;

/// Public entry point for registering integrations and creating sessions.
pub struct Harness {
    integrations: Arc<IntegrationRegistry>,
    sessions: Arc<SessionManager>,
}

impl Harness {
    /// Create a harness with an empty integration registry and a fresh
    /// [`SessionManager`].
    pub fn new() -> Self {
        Self {
            integrations: Arc::new(IntegrationRegistry::new()),
            sessions: Arc::new(SessionManager::default()),
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
    /// sessions, or (in future) restore persisted sessions without going
    /// through a [`SessionBuilder`].
    pub fn session_manager(&self) -> Arc<SessionManager> {
        self.sessions.clone()
    }

    /// Begin building a session using this harness's integration registry
    /// and session manager.
    pub fn session(&self) -> SessionBuilder {
        SessionBuilder::with_integrations_and_manager(
            self.integrations.clone(),
            self.sessions.clone(),
        )
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}
