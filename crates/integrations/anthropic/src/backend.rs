
//! Anthropic backend composition and registry factory.

use std::sync::Arc;

use async_trait::async_trait;
use harness_generic_backend::GenericModelBackend;
use harness_protocol::backend::{BackendCapabilities, BackendDescriptor};
use harness_protocol::ids::BackendId;
use harness_runtime::{traits::ExecutionBackend, IntegrationFactory};

use crate::client::AnthropicClient;
use crate::config::AnthropicConfig;

/// Convenience constructor for a generic backend backed by Anthropic.
pub struct AnthropicBackend;

impl AnthropicBackend {
    /// Construct an Anthropic-backed execution backend for direct injection.
    pub fn new(config: AnthropicConfig) -> GenericModelBackend {
        let recovery = config.recovery.clone();
        GenericModelBackend::new_with_recovery(Arc::new(AnthropicClient::new(config)), recovery)
    }
}

/// Registry factory for the `anthropic` integration family.
pub struct AnthropicFactory;

#[async_trait]
impl IntegrationFactory for AnthropicFactory {
    fn id(&self) -> &'static str {
        "anthropic"
    }

    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: BackendId::new(),
            name: "Anthropic".to_string(),
            description: "Anthropic Messages API via GenericModelBackend".to_string(),
            capabilities: BackendCapabilities {
                streaming: true,
                reasoning_stream: true,
                tool_calls: true,
                parallel_tool_calls: true,
                host_managed_tools: true,
                ..Default::default()
            },
        }
    }

    async fn create(
        &self,
        config: serde_json::Value,
    ) -> Result<Arc<dyn ExecutionBackend>, Box<dyn std::error::Error + Send + Sync>> {
        let config: AnthropicConfig = serde_json::from_value(config)?;
        Ok(Arc::new(AnthropicBackend::new(config)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_constructor_exposes_anthropic_capabilities() {
        let backend = AnthropicBackend::new(AnthropicConfig::new("test-key"));
        let capabilities = backend.capabilities();
        assert!(capabilities.streaming);
        assert!(capabilities.tool_calls);
        assert!(capabilities.host_managed_tools);
        assert_eq!(
            backend.recovery_policy(),
            &harness_generic_backend::RecoveryPolicy::default()
        );
    }

    #[test]
    fn direct_constructor_uses_configured_recovery_policy() {
        let mut config = AnthropicConfig::new("test-key");
        config.recovery.max_attempts = 5;
        let backend = AnthropicBackend::new(config);
        assert_eq!(backend.recovery_policy().max_attempts, 5);
    }

    #[tokio::test]
    async fn factory_constructs_backend_from_json() {
        let backend = AnthropicFactory
            .create(serde_json::json!({ "api_key": "test-key" }))
            .await
            .expect("valid Anthropic configuration");
        assert!(backend.capabilities().streaming);
    }
}
