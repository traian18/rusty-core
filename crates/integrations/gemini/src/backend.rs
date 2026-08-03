//! Gemini backend composition and registry factory.

use std::sync::Arc;

use async_trait::async_trait;
use harness_generic_backend::GenericModelBackend;
use harness_protocol::backend::{BackendCapabilities, BackendDescriptor};
use harness_protocol::ids::BackendId;
use harness_runtime::{traits::ExecutionBackend, IntegrationFactory};

use crate::client::GeminiClient;
use crate::config::GeminiConfig;

/// Convenience constructor for a generic backend backed by Gemini.
pub struct GeminiBackend;

impl GeminiBackend {
    pub fn new(config: GeminiConfig) -> GenericModelBackend {
        GenericModelBackend::new(Arc::new(GeminiClient::new(config)))
    }
}

/// Registry factory for the `gemini` integration family.
pub struct GeminiFactory;

#[async_trait]
impl IntegrationFactory for GeminiFactory {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: BackendId::new(),
            name: "Gemini".to_string(),
            description: "Gemini streamGenerateContent API via GenericModelBackend".to_string(),
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
        let config: GeminiConfig = serde_json::from_value(config)?;
        Ok(Arc::new(GeminiBackend::new(config)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_constructor_exposes_gemini_capabilities() {
        let backend = GeminiBackend::new(GeminiConfig::new("test-key"));
        let capabilities = backend.capabilities();
        assert!(capabilities.streaming);
        assert!(capabilities.tool_calls);
        assert!(capabilities.host_managed_tools);
    }

    #[tokio::test]
    async fn factory_constructs_backend_from_json() {
        let backend = GeminiFactory
            .create(serde_json::json!({ "api_key": "test-key" }))
            .await
            .expect("valid Gemini configuration");
        assert!(backend.capabilities().streaming);
    }
}
