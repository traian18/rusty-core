//! OpenAI backend composition and registry factory.

use std::sync::Arc;

use async_trait::async_trait;
use harness_generic_backend::GenericModelBackend;
use harness_protocol::backend::{BackendCapabilities, BackendDescriptor};
use harness_protocol::ids::BackendId;
use harness_runtime::{traits::ExecutionBackend, IntegrationFactory};

use crate::client::OpenAiClient;
use crate::config::OpenAiConfig;

/// Convenience constructor for a generic backend backed by OpenAI.
pub struct OpenAiBackend;

impl OpenAiBackend {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(config: OpenAiConfig) -> GenericModelBackend {
        let recovery = config.recovery.clone();
        GenericModelBackend::new_with_recovery(Arc::new(OpenAiClient::new(config)), recovery)
    }
}

/// Registry factory for the `openai` integration family.
pub struct OpenAiFactory;

#[async_trait]
impl IntegrationFactory for OpenAiFactory {
    fn id(&self) -> &'static str {
        "openai"
    }

    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: BackendId::new(),
            name: "OpenAI".to_string(),
            description: "OpenAI Chat Completions API via GenericModelBackend".to_string(),
            capabilities: BackendCapabilities {
                streaming: true,
                reasoning_stream: false,
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
        let config: OpenAiConfig = serde_json::from_value(config)?;
        Ok(Arc::new(OpenAiBackend::new(config)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_constructor_exposes_openai_capabilities() {
        let backend = OpenAiBackend::new(OpenAiConfig::new("test-key"));
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
        let mut config = OpenAiConfig::new("test-key");
        config.recovery.max_attempts = 5;
        let backend = OpenAiBackend::new(config);
        assert_eq!(backend.recovery_policy().max_attempts, 5);
    }

    #[tokio::test]
    async fn factory_constructs_backend_from_json() {
        let backend = OpenAiFactory
            .create(serde_json::json!({ "api_key": "test-key" }))
            .await
            .expect("valid OpenAI configuration");
        assert!(backend.capabilities().streaming);
    }
}
