//! Copilot inference APIs. There is no provider-owned agent or CLI execution.
use crate::{auth::CopilotAuth, config::GitHubCopilotConfig};
use async_trait::async_trait;
use harness_generic_backend::GenericModelBackend;
use harness_integration_openai::{client::OpenAiClient, OpenAiConfig, OpenAiFactory};
use harness_integration_openai_responses::{OpenAiResponsesClient, OpenAiResponsesConfig};
use harness_model::{
    ModelCapabilities, ModelClient, ModelError, ModelEventSender, ModelRequest, ModelResult,
};
use harness_protocol::backend::BackendDescriptor;
use harness_runtime::{traits::ExecutionBackend, IntegrationFactory};
use std::sync::Arc;

struct CopilotClient {
    chat: OpenAiClient,
    responses: OpenAiResponsesClient,
    default_model: String,
}
#[async_trait]
impl ModelClient for CopilotClient {
    fn capabilities(&self) -> ModelCapabilities {
        self.responses.capabilities()
    }
    async fn stream(
        &self,
        mut request: ModelRequest,
        events: ModelEventSender,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ModelResult, ModelError> {
        let model = request
            .model
            .as_deref()
            .filter(|m| *m != "auto" && !m.is_empty())
            .unwrap_or(&self.default_model)
            .to_owned();
        request.model = Some(model.clone());
        if uses_responses(&model) {
            self.responses.stream(request, events, cancel).await
        } else {
            self.chat.stream(request, events, cancel).await
        }
    }
}
fn uses_responses(model: &str) -> bool {
    model
        .strip_prefix("gpt-")
        .and_then(|rest| rest.split(['.', '-']).next())
        .and_then(|n| n.parse::<u32>().ok())
        .is_some_and(|generation| generation >= 5)
        && !model.starts_with("gpt-5-mini")
}
fn api_root(host: &str) -> Result<String, String> {
    let host = host.trim_start_matches("https://").trim_end_matches('/');
    if host == "github.com" {
        return Ok("https://api.githubcopilot.com".into());
    }
    if host.ends_with(".ghe.com")
        && host
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-')
    {
        return Ok(format!("https://copilot-api.{host}"));
    }
    Err("Unsupported Copilot host; configure github.com or a GitHub Enterprise Cloud .ghe.com host.".into())
}
pub struct GitHubCopilotBackend;
impl GitHubCopilotBackend {
    pub fn build(config: GitHubCopilotConfig) -> Result<GenericModelBackend, String> {
        let root = api_root(&config.github_host)?;
        let auth = Arc::new(CopilotAuth::new(
            config.credentials_path,
            config.github_host,
        ));
        let mut chat = OpenAiConfig::new("");
        chat.base_url = root.clone();
        chat.default_model = config.default_model.clone();
        chat.supports_reasoning = true;
        let mut responses = OpenAiResponsesConfig::new("");
        responses.base_url = root;
        responses.default_model = config.default_model.clone();
        Ok(GenericModelBackend::new(Arc::new(CopilotClient {
            chat: OpenAiClient::new(chat).with_auth(auth.clone()),
            responses: OpenAiResponsesClient::new(responses).with_auth(auth),
            default_model: config.default_model,
        })))
    }
}
pub struct GitHubCopilotFactory;
#[async_trait]
impl IntegrationFactory for GitHubCopilotFactory {
    fn id(&self) -> &'static str {
        "github-copilot"
    }
    fn descriptor(&self) -> BackendDescriptor {
        let mut descriptor = OpenAiFactory.descriptor();
        descriptor.name = "GitHub Copilot subscription".into();
        descriptor.description = "Copilot model APIs; all tools executed by the harness".into();
        descriptor
    }
    async fn create(
        &self,
        config: serde_json::Value,
    ) -> Result<Arc<dyn ExecutionBackend>, Box<dyn std::error::Error + Send + Sync>> {
        let config: GitHubCopilotConfig = if config.is_null() {
            Default::default()
        } else {
            serde_json::from_value(config)?
        };
        Ok(Arc::new(GitHubCopilotBackend::build(config)?))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn factory_has_no_provider_execution_or_cli_configuration() {
        let backend = GitHubCopilotFactory
            .create(serde_json::json!({}))
            .await
            .unwrap();
        assert!(backend.capabilities().tool_calls && backend.capabilities().host_managed_tools);
        assert!(!backend.capabilities().backend_managed_tools);
        assert!(GitHubCopilotFactory
            .create(serde_json::json!({"binary_path":"copilot"}))
            .await
            .is_err());
    }
    #[test]
    fn inference_routes_match_model_api_families() {
        assert!(uses_responses("gpt-5.5"));
        assert!(uses_responses("gpt-6-sol"));
        assert!(!uses_responses("gpt-5-mini"));
        assert!(!uses_responses("claude-haiku-4.5"));
        assert_eq!(
            api_root("github.com").unwrap(),
            "https://api.githubcopilot.com"
        );
        assert_eq!(
            api_root("https://tenant.ghe.com").unwrap(),
            "https://copilot-api.tenant.ghe.com"
        );
        assert!(api_root("evil.invalid/path").is_err());
    }
    #[tokio::test]
    async fn both_inference_protocols_return_tool_requests_without_executing_them() {
        use harness_model::ModelEvent;
        use harness_protocol::{
            ids::{MessageId, Timestamp},
            messages::{AgentMessage, ContentBlock, MessageRole},
        };
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        for model in ["gpt-5.5", "claude-haiku-4.5"] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let root = format!("http://{}", listener.local_addr().unwrap());
            let responses = uses_responses(model);
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let (headers, body) = loop {
                    let mut chunk = [0; 4096];
                    let size = socket.read(&mut chunk).await.unwrap();
                    assert!(size > 0);
                    bytes.extend_from_slice(&chunk[..size]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_string();
                        let size: usize = headers
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + size {
                            break (
                                headers,
                                serde_json::from_slice::<serde_json::Value>(
                                    &bytes[end + 4..end + 4 + size],
                                )
                                .unwrap(),
                            );
                        }
                    }
                };
                assert!(headers.starts_with(if responses {
                    "POST /responses "
                } else {
                    "POST /chat/completions "
                }));
                assert!(headers
                    .to_lowercase()
                    .contains("authorization: bearer fixture"));
                assert!(headers.to_lowercase().contains("x-initiator: user"));
                assert!(headers
                    .to_lowercase()
                    .contains("openai-intent: conversation-edits"));
                // Untyped options cannot introduce hosted execution, even with no tools granted.
                assert!(
                    body.get("tools").is_none() || body["tools"].as_array().unwrap().is_empty()
                );
                let frames = if responses {
                    vec![
                        serde_json::json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call_fixture","name":"run_command"}}),
                        serde_json::json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","arguments":"{\"program\":\"npm\"}"}}),
                        serde_json::json!({"type":"response.completed","response":{"model":"gpt-5.5"}}),
                    ]
                } else {
                    vec![
                        serde_json::json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_fixture","type":"function","function":{"name":"run_command","arguments":"{\"program\":\"npm\"}"}}]},"finish_reason":null}]}),
                        serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
                    ]
                };
                let mut sse: String = frames
                    .iter()
                    .map(|frame| format!("data: {frame}\n\n"))
                    .collect();
                if !responses {
                    sse.push_str("data: [DONE]\n\n");
                }
                socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}",sse.len()).as_bytes()).await.unwrap();
            });
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("config.json");
            std::fs::write(
                &path,
                serde_json::json!({"lastLoggedInUser":{"login":"test","token":"fixture"}})
                    .to_string(),
            )
            .unwrap();
            let auth = Arc::new(CopilotAuth::new(Some(path), "github.com".into()));
            let mut chat = OpenAiConfig::new("");
            chat.base_url = root.clone();
            let mut api = OpenAiResponsesConfig::new("");
            api.base_url = root;
            let client = CopilotClient {
                chat: OpenAiClient::new(chat).with_auth(auth.clone()),
                responses: OpenAiResponsesClient::new(api).with_auth(auth),
                default_model: "gpt-4.1".into(),
            };
            let request = ModelRequest {
                system_prompt: String::new(),
                messages: vec![AgentMessage {
                    id: MessageId::new(),
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text {
                        text: "hello".into(),
                    }],
                    created_at: Timestamp::now(),
                }],
                tools: vec![],
                model: Some(model.into()),
                max_tokens: Some(64),
                temperature: None,
                stop_sequences: vec![],
                extended_thinking: false,
                reasoning_effort: None,
                response_format: None,
                provider_options: serde_json::json!({"openai":{"tools":[{"type":"web_search"}]},"openai-responses":{"tools":[{"type":"web_search"}]}}),
            };
            let (events, mut receiver) = tokio::sync::mpsc::channel(32);
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                client.stream(request, events, tokio_util::sync::CancellationToken::new()),
            )
            .await
            .unwrap()
            .unwrap();
            server.await.unwrap();
            let mut requested = false;
            while let Ok(event) = receiver.try_recv() {
                if let ModelEvent::ToolCallCompleted { name, input, .. } = event {
                    assert_eq!(name, "run_command");
                    assert_eq!(input["program"], "npm");
                    requested = true;
                }
            }
            assert!(requested, "tool request must be returned to the harness");
        }
    }
}
