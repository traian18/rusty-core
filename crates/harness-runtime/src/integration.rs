//! Dynamic integration factory and registry: lets backends be constructed at
//! runtime from provider-specific JSON config, rather than compile time.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use harness_protocol::backend::BackendDescriptor;
use serde_json::Value;
use thiserror::Error;

use crate::traits::ExecutionBackend;

/// Errors raised when registering or constructing an integration.
#[derive(Debug, Error)]
pub enum IntegrationError {
    /// Requested identifier has no registered factory.
    #[error("integration is not registered: {0}")]
    NotRegistered(String),
    /// Registry lock poisoned by a panicking thread.
    #[error("integration registry lock was poisoned")]
    RegistryPoisoned,
    /// Factory rejected its config or failed to construct a backend.
    #[error("failed to create integration {integration}: {message}")]
    Creation {
        integration: String,
        message: String,
    },
}

/// Provider-owned constructor for dynamically configured execution backends.
/// Implementors own how their config maps to a running backend.
#[async_trait]
pub trait IntegrationFactory: Send + Sync {
    /// Stable integration family identifier, e.g. `"anthropic"`.
    fn id(&self) -> &'static str;
    /// Descriptor for the backend family.
    fn descriptor(&self) -> BackendDescriptor;
    /// Build a fresh backend from provider-specific JSON configuration.
    async fn create(
        &self,
        config: Value,
    ) -> Result<Arc<dyn ExecutionBackend>, Box<dyn std::error::Error + Send + Sync>>;
}

/// Thread-safe registry of integration factories, keyed by factory `id`.
#[derive(Clone, Default)]
pub struct IntegrationRegistry {
    factories: Arc<RwLock<HashMap<String, Arc<dyn IntegrationFactory>>>>,
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

    /// Merge all factories from another registry into this one; entries with
    /// the same identifier are replaced.
    pub fn extend_from(&self, other: &Self) -> Result<(), IntegrationError> {
        let factories = other
            .factories
            .read()
            .map_err(|_| IntegrationError::RegistryPoisoned)?
            .clone();
        self.factories
            .write()
            .map_err(|_| IntegrationError::RegistryPoisoned)?
            .extend(factories);
        Ok(())
    }

    /// Look up a registered factory without constructing a backend.
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

    /// Resolve a factory and construct a fresh backend from the given config.
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
