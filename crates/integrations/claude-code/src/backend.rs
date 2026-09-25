//! Subscription inference through the Anthropic Messages API; never starts Claude's agent.
use crate::{auth::ClaudeAuth, config::ClaudeCodeConfig};
use async_trait::async_trait;
use harness_generic_backend::GenericModelBackend;
use harness_integration_anthropic::{client::AnthropicClient, AnthropicConfig, AnthropicFactory};
use harness_protocol::backend::BackendDescriptor;
use harness_runtime::{traits::ExecutionBackend, IntegrationFactory};
use std::sync::Arc;

pub struct ClaudeCodeBackend;
impl ClaudeCodeBackend {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(config: ClaudeCodeConfig) -> GenericModelBackend {
        let mut api = AnthropicConfig::new("");
        api.default_model = config.default_model;
        let recovery = api.recovery.clone();
        let client =
            AnthropicClient::new(api).with_auth(Arc::new(ClaudeAuth::new(config.credentials_path)));
        GenericModelBackend::new_with_recovery(Arc::new(client), recovery)
    }
}
pub struct ClaudeCodeFactory;
#[async_trait]
impl IntegrationFactory for ClaudeCodeFactory {
    fn id(&self) -> &'static str {
        "claude-code"
    }
    fn descriptor(&self) -> BackendDescriptor {
        let mut descriptor = AnthropicFactory.descriptor();
        descriptor.name = "Claude subscription".into();
        descriptor.description =
            "Anthropic Messages inference; all tools executed by the harness".into();
        descriptor
    }
    async fn create(
        &self,
        config: serde_json::Value,
    ) -> Result<Arc<dyn ExecutionBackend>, Box<dyn std::error::Error + Send + Sync>> {
        let config: ClaudeCodeConfig = if config.is_null() {
            Default::default()
        } else {
            serde_json::from_value(config)?
        };
        Ok(Arc::new(ClaudeCodeBackend::new(config)))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn subscription_backend_only_proposes_host_tools() {
        let backend = ClaudeCodeFactory
            .create(serde_json::json!({}))
            .await
            .unwrap();
        assert!(backend.capabilities().host_managed_tools);
        assert!(backend.capabilities().tool_calls);
        assert!(!backend.capabilities().backend_managed_tools);
        assert!(ClaudeCodeFactory
            .create(serde_json::json!({"permission_mode":"bypassPermissions"}))
            .await
            .is_err());
    }
    #[tokio::test]
    #[ignore = "uses the locally signed-in Claude subscription for one inference request, without tools"]
    async fn live_subscription_inference_without_tools() {
        use harness_protocol::{
            backend::ExecutionRequest,
            ids::{MessageId, RequestId, RunId, Timestamp},
            messages::{AgentMessage, ContentBlock, MessageRole},
        };
        let backend = ClaudeCodeBackend::new(ClaudeCodeConfig::default());
        let (events, _receiver) = tokio::sync::broadcast::channel(128);
        backend
            .execute(
                ExecutionRequest {
                    request_id: RequestId::new(),
                    run_id: RunId::new(),
                    system_prompt: "You are a coding assistant. Respond briefly.".into(),
                    messages: vec![AgentMessage {
                        id: MessageId::new(),
                        role: MessageRole::User,
                        content: vec![ContentBlock::Text {
                            text: "Reply with only OK.".into(),
                        }],
                        created_at: Timestamp::now(),
                    }],
                    tools: vec![],
                    extended_thinking: false,
                    params: harness_protocol::backend::ExecutionParams {
                        max_tokens: Some(32),
                        ..Default::default()
                    },
                },
                events,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .expect("subscription inference should succeed");
    }
}
