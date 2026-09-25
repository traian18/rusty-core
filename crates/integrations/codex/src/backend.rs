//! ChatGPT subscription inference through the Responses API.
use crate::{auth::CodexAuth, config::CodexConfig};
use async_trait::async_trait;
use harness_generic_backend::GenericModelBackend;
use harness_integration_openai_responses::{
    OpenAiResponsesClient, OpenAiResponsesConfig, OpenAiResponsesFactory,
};
use harness_protocol::backend::BackendDescriptor;
use harness_runtime::{traits::ExecutionBackend, IntegrationFactory};
use std::sync::Arc;

pub struct CodexBackend;
impl CodexBackend {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(config: CodexConfig) -> GenericModelBackend {
        let mut responses = OpenAiResponsesConfig::new("");
        responses.base_url = "https://chatgpt.com/backend-api/codex".into();
        responses.default_model = config.default_model;
        responses
            .extra_headers
            .insert("originator".into(), "rusty-harness".into());
        let recovery = responses.recovery.clone();
        let client = OpenAiResponsesClient::new(responses)
            .with_chatgpt_auth(Arc::new(CodexAuth::new(config.auth_path)));
        GenericModelBackend::new_with_recovery(Arc::new(client), recovery)
    }
}

pub struct CodexFactory;
#[async_trait]
impl IntegrationFactory for CodexFactory {
    fn id(&self) -> &'static str {
        "codex"
    }
    fn descriptor(&self) -> BackendDescriptor {
        let mut descriptor = OpenAiResponsesFactory.descriptor();
        descriptor.name = "Codex subscription".into();
        descriptor.description =
            "ChatGPT subscription Responses API; all tools executed by the harness".into();
        descriptor
    }
    async fn create(
        &self,
        config: serde_json::Value,
    ) -> Result<Arc<dyn ExecutionBackend>, Box<dyn std::error::Error + Send + Sync>> {
        let config: CodexConfig = if config.is_null() {
            Default::default()
        } else {
            serde_json::from_value(config)?
        };
        Ok(Arc::new(CodexBackend::new(config)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn subscription_backend_only_proposes_host_tools() {
        let backend = CodexFactory.create(serde_json::json!({})).await.unwrap();
        let caps = backend.capabilities();
        assert!(caps.tool_calls && caps.host_managed_tools);
        assert!(!caps.backend_managed_tools);
        assert!(!CodexFactory.descriptor().capabilities.backend_managed_tools);
    }
    #[tokio::test]
    async fn cli_execution_configuration_is_rejected() {
        for config in [
            serde_json::json!({"binary_path":"codex"}),
            serde_json::json!({"dangerously_bypass":true}),
        ] {
            assert!(CodexFactory.create(config).await.is_err());
        }
    }
}
