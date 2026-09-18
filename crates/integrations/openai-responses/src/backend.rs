//! OpenAI Responses API backend composition and registry factory.

use std::sync::Arc;

use async_trait::async_trait;
use harness_generic_backend::GenericModelBackend;
use harness_protocol::backend::{BackendCapabilities, BackendDescriptor};
use harness_protocol::ids::BackendId;
use harness_runtime::{traits::ExecutionBackend, IntegrationFactory};

use crate::client::OpenAiResponsesClient;
use crate::config::OpenAiResponsesConfig;

/// Convenience constructor for a generic backend backed by the OpenAI
/// Responses API.
pub struct OpenAiResponsesBackend;

impl OpenAiResponsesBackend {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(config: OpenAiResponsesConfig) -> GenericModelBackend {
        let recovery = config.recovery.clone();
        GenericModelBackend::new_with_recovery(Arc::new(OpenAiResponsesClient::new(config)), recovery)
    }
}

/// Registry factory for the `openai-responses` integration family.
pub struct OpenAiResponsesFactory;

#[async_trait]
impl IntegrationFactory for OpenAiResponsesFactory {
    fn id(&self) -> &'static str {
        "openai-responses"
    }

    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: BackendId::new(),
            name: "OpenAI Responses".to_string(),
            description: "OpenAI Responses API via GenericModelBackend".to_string(),
            capabilities: BackendCapabilities {
                streaming: true,
                reasoning_stream: true,
                tool_calls: true,
                parallel_tool_calls: true,
                host_managed_tools: true,
                structured_output: false,
                ..Default::default()
            },
        }
    }

    async fn create(
        &self,
        config: serde_json::Value,
    ) -> Result<Arc<dyn ExecutionBackend>, Box<dyn std::error::Error + Send + Sync>> {
        let config: OpenAiResponsesConfig = serde_json::from_value(config)?;
        Ok(Arc::new(OpenAiResponsesBackend::new(config)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_constructor_exposes_responses_capabilities() {
        let backend = OpenAiResponsesBackend::new(OpenAiResponsesConfig::new("test-key"));
        let capabilities = backend.capabilities();
        assert!(capabilities.streaming);
        assert!(capabilities.tool_calls);
        assert!(capabilities.host_managed_tools);
        assert_eq!(
            backend.recovery_policy(),
            &harness_generic_backend::RecoveryPolicy::default()
        );
    }

    #[tokio::test]
    async fn factory_constructs_backend_from_json() {
        let backend = OpenAiResponsesFactory
            .create(serde_json::json!({ "api_key": "test-key" }))
            .await
            .expect("valid OpenAI Responses configuration");
        assert!(backend.capabilities().streaming);
    }
}
