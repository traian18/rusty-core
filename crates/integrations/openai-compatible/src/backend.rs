//! OpenAI-compatible backend composition and registry factory.
//!
//! Reuses `harness_integration_openai::OpenAiClient` directly — see
//! `OpenAiCompatibleConfig::into_openai_config` for the whole mechanism.
//! This crate contains no wire format or SSE parsing logic of its own.

use std::sync::Arc;

use async_trait::async_trait;
use harness_generic_backend::GenericModelBackend;
use harness_integration_openai::OpenAiClient;
use harness_protocol::backend::{BackendCapabilities, BackendDescriptor};
use harness_protocol::ids::BackendId;
use harness_runtime::{traits::ExecutionBackend, IntegrationFactory};

use crate::config::OpenAiCompatibleConfig;

pub struct OpenAiCompatibleBackend;

impl OpenAiCompatibleBackend {
    pub fn new(config: OpenAiCompatibleConfig) -> GenericModelBackend {
        GenericModelBackend::new(Arc::new(OpenAiClient::new(config.into_openai_config())))
    }
}

/// Registry factory for the `openai-compatible` integration family.
pub struct OpenAiCompatibleFactory;

#[async_trait]
impl IntegrationFactory for OpenAiCompatibleFactory {
    fn id(&self) -> &'static str {
        "openai-compatible"
    }

    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: BackendId::new(),
            name: "OpenAI-compatible".to_string(),
            description: "Any OpenAI-Chat-Completions-compatible endpoint via GenericModelBackend"
                .to_string(),
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
        let config: OpenAiCompatibleConfig = serde_json::from_value(config)?;
        if config.base_url.is_empty() {
            return Err("openai-compatible config requires a non-empty base_url".into());
        }
        if config.model.is_empty() {
            return Err("openai-compatible config requires a non-empty model".into());
        }
        Ok(Arc::new(OpenAiCompatibleBackend::new(config)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_constructor_exposes_capabilities() {
        let backend = OpenAiCompatibleBackend::new(OpenAiCompatibleConfig::new(
            "http://localhost:11434/v1",
            "llama3",
        ));
        let capabilities = backend.capabilities();
        assert!(capabilities.streaming);
        assert!(capabilities.tool_calls);
    }

    #[tokio::test]
    async fn factory_constructs_backend_from_json() {
        let backend = OpenAiCompatibleFactory
            .create(serde_json::json!({
                "base_url": "http://localhost:11434/v1",
                "model": "llama3"
            }))
            .await
            .expect("valid config");
        assert!(backend.capabilities().streaming);
    }

    #[tokio::test]
    async fn factory_rejects_a_missing_base_url() {
        let result = OpenAiCompatibleFactory
            .create(serde_json::json!({ "base_url": "", "model": "llama3" }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn factory_rejects_a_missing_model() {
        let result = OpenAiCompatibleFactory
            .create(serde_json::json!({ "base_url": "http://localhost:11434/v1", "model": "" }))
            .await;
        assert!(result.is_err());
    }
}
