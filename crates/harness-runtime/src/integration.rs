//! Dynamic integration factory and registry support.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use harness_protocol::backend::BackendDescriptor;
use serde_json::Value;
use thiserror::Error;

use crate::traits::ExecutionBackend;

/// Error returned while registering or constructing an integration.
#[derive(Debug, Error)]
pub enum IntegrationError {
    /// No factory was registered under the requested identifier.
    #[error("integration is not registered: {0}")]
    NotRegistered(String),
    /// A registry lock was poisoned.
    #[error("integration registry lock was poisoned")]
    RegistryPoisoned,
    /// The factory rejected its configuration or failed to construct a backend.
    #[error("failed to create integration {integration}: {message}")]
    Creation {
        integration: String,
        message: String,
    },
}

/// Provider-owned constructor for dynamically configured execution backends.
#[async_trait]
pub trait IntegrationFactory: Send + Sync {
    /// Stable integration family identifier, such as `"anthropic"`.
    fn id(&self) -> &'static str;

    /// Descriptor for the backend family.
    fn descriptor(&self) -> BackendDescriptor;

    /// Construct a fresh backend from provider-specific JSON configuration.
    async fn create(
        &self,
        config: Value,
    ) -> Result<Arc<dyn ExecutionBackend>, Box<dyn std::error::Error + Send + Sync>>;
}

/// Thread-safe registry of integration factories.
#[derive(Default)]
pub struct IntegrationRegistry {
    factories: RwLock<HashMap<String, Arc<dyn IntegrationFactory>>>,
}

impl IntegrationRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register or replace the factory for its stable identifier.
    pub fn register(&self, factory: Arc<dyn IntegrationFactory>) -> Result<(), IntegrationError> {
        let mut factories = self
            .factories
            .write()
            .map_err(|_| IntegrationError::RegistryPoisoned)?;
        factories.insert(factory.id().to_string(), factory);
        Ok(())
    }

    /// Return a registered factory without constructing a backend.
    pub fn get(
        &self,
        integration: &str,
    ) -> Result<Option<Arc<dyn IntegrationFactory>>, IntegrationError> {
        let factories = self
            .factories
            .read()
            .map_err(|_| IntegrationError::RegistryPoisoned)?;
        Ok(factories.get(integration).cloned())
    }

    /// Resolve a factory and construct a fresh backend.
    pub async fn create(
        &self,
        integration: &str,
        config: Value,
    ) -> Result<Arc<dyn ExecutionBackend>, IntegrationError> {
        let factory = self
            .get(integration)?
            .ok_or_else(|| IntegrationError::NotRegistered(integration.to_string()))?;

        factory
            .create(config)
            .await
            .map_err(|error| IntegrationError::Creation {
                integration: integration.to_string(),
                message: error.to_string(),
            })
    }
}
