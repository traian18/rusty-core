//! Top-level public harness entry point.

use std::sync::Arc;

use harness_runtime::{IntegrationError, IntegrationFactory, IntegrationRegistry};

use crate::session_builder::SessionBuilder;

/// Public entry point for registering integrations and creating sessions.
pub struct Harness {
    integrations: Arc<IntegrationRegistry>,
}

impl Harness {
    /// Create a harness with an empty integration registry.
    pub fn new() -> Self {
        Self {
            integrations: Arc::new(IntegrationRegistry::new()),
        }
    }

    /// Register a dynamically constructible integration family.
    pub fn register_integration(
        &self,
        factory: Arc<dyn IntegrationFactory>,
    ) -> Result<(), IntegrationError> {
        self.integrations.register(factory)
    }

    /// Begin building a session using this harness's integration registry.
    pub fn session(&self) -> SessionBuilder {
        SessionBuilder::with_integrations(self.integrations.clone())
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}
