//! Copilot inference APIs. There is no provider-owned agent or CLI execution.
use crate::{auth::CopilotAuth, catalog::Catalog, config::GitHubCopilotConfig};
use async_trait::async_trait;
use harness_generic_backend::GenericModelBackend;
use harness_integration_anthropic::{client::AnthropicClient, AnthropicConfig};
use harness_integration_openai::{client::OpenAiClient, OpenAiConfig, OpenAiFactory};
use harness_integration_openai_responses::{OpenAiResponsesClient, OpenAiResponsesConfig};
use harness_model::{
    auth::InferenceAuth, ModelCapabilities, ModelClient, ModelError, ModelEventSender,
    ModelRequest, ModelResult,
};
use harness_protocol::backend::BackendDescriptor;
use harness_runtime::{traits::ExecutionBackend, IntegrationFactory};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

/// How long an account's model catalog is trusted before it is fetched again.
const CATALOG_TTL: Duration = Duration::from_secs(300);
const CATALOG_TIMEOUT: Duration = Duration::from_secs(10);

/// Where and how to read the account's `/models` catalog.
struct CatalogSource {
    url: String,
    auth: Arc<CopilotAuth>,
    http: reqwest::Client,
    cached: Mutex<Option<(Instant, Arc<Catalog>)>>,
}
impl CatalogSource {
    fn new(root: &str, auth: Arc<CopilotAuth>) -> Self {
        Self {
            url: format!("{root}/models"),
            auth,
            http: reqwest::Client::new(),
            cached: Mutex::new(None),
        }
    }
    /// The catalog, or `None` when it cannot be read; callers then let the API
    /// judge the model rather than failing on a catalog problem.
    async fn get(&self) -> Option<Arc<Catalog>> {
        let mut cached = self.cached.lock().await;
        if let Some((fetched, catalog)) = cached.as_ref() {
            if fetched.elapsed() < CATALOG_TTL {
                return Some(catalog.clone());
            }
        }
        let fetched = self.fetch().await.map(Arc::new);
        match &fetched {
            Some(catalog) => *cached = Some((Instant::now(), catalog.clone())),
            // Keep serving a stale catalog over none at all.
            None => return cached.as_ref().map(|(_, catalog)| catalog.clone()),
        }
        fetched
    }
    async fn fetch(&self) -> Option<Catalog> {
        let headers = self.auth.headers(&serde_json::json!({})).await.ok()?;
        let response = self
            .http
            .get(&self.url)
            .headers(headers)
            .timeout(CATALOG_TIMEOUT)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?;
        Catalog::parse(&response.json().await.ok()?)
    }
}

struct CopilotClient {
    chat: OpenAiClient,
    responses: OpenAiResponsesClient,
    messages: AnthropicClient,
    default_model: String,
    catalog: Option<CatalogSource>,
}
impl CopilotClient {
    /// Picks the model to call. "auto" resolves to one this account lists
    /// (Copilot rejects models outside its plan or policy with
    /// `model_not_supported`). A model the user chose is always sent as-is:
    /// the catalog is a best-effort guess about the account, Copilot's answer
    /// is the truth, so the catalog never blocks a request on its own.
    async fn resolve_model(&self, requested: Option<&str>) -> (String, Option<Arc<Catalog>>) {
        let catalog = match &self.catalog {
            Some(source) => source.get().await,
            None => None,
        };
        let model = requested
            .map(str::to_owned)
            .or_else(|| {
                catalog
                    .as_deref()
                    .and_then(|known| known.pick_auto(&self.default_model))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| self.default_model.clone());
        (model, catalog)
    }
}
/// Adds the direct API's models to its own `model_not_supported` rejection.
/// Copilot CLI can expose a different catalog for the same signed-in account.
fn explain_rejection(error: ModelError, model: &str, catalog: Option<&Catalog>) -> ModelError {
    let ModelError::BackendError { message, code } = error else {
        return error;
    };
    if !(code == "model_not_supported" || message.contains("model_not_supported")) {
        return ModelError::BackendError { message, code };
    }
    let listed = catalog.map(Catalog::selectable_ids).unwrap_or_default();
    let hint = if listed.is_empty() {
        String::new()
    } else {
        let shown = listed
            .iter()
            .take(12)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        let more = listed.len().saturating_sub(12);
        let more = if more > 0 {
            format!(" (+{more} more)")
        } else {
            String::new()
        };
        format!(" Models this direct API endpoint lists: {shown}{more}.")
    };
    ModelError::BackendError {
        message: format!(
            "GitHub Copilot rejected model \"{model}\" for this API sign-in.{hint} Refresh Copilot models or sign in again in Rusty's settings; Copilot CLI sign-in is separate. Original response: {}",
            message.trim()
        ),
        code,
    }
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
        let requested = request
            .model
            .as_deref()
            .filter(|m| *m != "auto" && !m.is_empty());
        let (model, catalog) = self.resolve_model(requested).await;
        request.model = Some(model.clone());
        if let Some(limit) = catalog
            .as_deref()
            .and_then(|known| known.get(&model))
            .and_then(|entry| entry.max_output_tokens)
        {
            request.max_tokens = Some(request.max_tokens.unwrap_or(limit).min(limit));
        }
        let result = match inference_route(&model, catalog.as_deref()) {
            InferenceRoute::Messages => self.messages.stream(request, events, cancel).await,
            InferenceRoute::Responses => self.responses.stream(request, events, cancel).await,
            InferenceRoute::Chat => self.chat.stream(request, events, cancel).await,
        };
        result.map_err(|error| explain_rejection(error, &model, catalog.as_deref()))
    }
}
#[derive(Debug, PartialEq, Eq)]
enum InferenceRoute {
    Chat,
    Responses,
    Messages,
}
fn inference_route(model: &str, catalog: Option<&Catalog>) -> InferenceRoute {
    // OpenCode prefers the native Messages endpoint when Copilot advertises
    // it, followed by Responses and Chat Completions.
    if let Some(entry) = catalog.and_then(|known| known.get(model)) {
        if entry
            .endpoints
            .iter()
            .any(|endpoint| endpoint == "/v1/messages")
        {
            return InferenceRoute::Messages;
        }
        if entry
            .endpoints
            .iter()
            .any(|endpoint| endpoint == "/responses")
        {
            return InferenceRoute::Responses;
        }
        if entry
            .endpoints
            .iter()
            .any(|endpoint| endpoint == "/chat/completions")
        {
            return InferenceRoute::Chat;
        }
    }
    if uses_responses(model) {
        InferenceRoute::Responses
    } else {
        InferenceRoute::Chat
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
pub fn api_root(host: &str) -> Result<String, String> {
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
        let catalog = CatalogSource::new(&root, auth.clone());
        let mut chat = OpenAiConfig::new("");
        chat.base_url = root.clone();
        chat.default_model = config.default_model.clone();
        chat.supports_reasoning = true;
        let mut responses = OpenAiResponsesConfig::new("");
        responses.base_url = root.clone();
        responses.default_model = config.default_model.clone();
        let mut messages = AnthropicConfig::new("");
        messages.base_url = root;
        messages.default_model = config.default_model.clone();
        Ok(GenericModelBackend::new(Arc::new(CopilotClient {
            chat: OpenAiClient::new(chat).with_auth(auth.clone()),
            responses: OpenAiResponsesClient::new(responses).with_auth(auth.clone()),
            messages: AnthropicClient::new(messages).with_auth(auth),
            default_model: config.default_model,
            catalog: Some(catalog),
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
    #[ignore = "requires a live direct Copilot OAuth credential"]
    async fn live_haiku_with_direct_oauth() {
        let path = std::env::var_os("RUSTY_COPILOT_LIVE_AUTH_PATH")
            .map(std::path::PathBuf::from)
            .expect("set RUSTY_COPILOT_LIVE_AUTH_PATH to the native credential metadata path");
        let (host, _) = crate::credentials::credential_account(&path).unwrap();
        let root = api_root(&host).unwrap();
        let auth = Arc::new(CopilotAuth::new(Some(path), host));
        let mut chat = OpenAiConfig::new("");
        chat.base_url = root.clone();
        let mut responses = OpenAiResponsesConfig::new("");
        responses.base_url = root.clone();
        let mut messages = AnthropicConfig::new("");
        messages.base_url = root.clone();
        let client = CopilotClient {
            chat: OpenAiClient::new(chat).with_auth(auth.clone()),
            responses: OpenAiResponsesClient::new(responses).with_auth(auth.clone()),
            messages: AnthropicClient::new(messages).with_auth(auth.clone()),
            default_model: "claude-haiku-4.5".into(),
            catalog: Some(CatalogSource::new(&root, auth)),
        };
        let catalog = client
            .catalog
            .as_ref()
            .unwrap()
            .get()
            .await
            .expect("live model catalog");
        assert!(
            catalog.get("claude-haiku-4.5").is_some(),
            "the direct sign-in must expose Haiku"
        );
        println!(
            "Live Haiku route: {:?}",
            inference_route("claude-haiku-4.5", Some(&catalog))
        );
        let (result, events) = ask_with_events(&client, Some("claude-haiku-4.5"))
            .await
            .unwrap();
        let text = events
            .iter()
            .filter_map(|event| match event {
                harness_model::ModelEvent::TextDelta { delta } => Some(delta.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert!(!text.trim().is_empty());
        println!(
            "Haiku streamed text successfully; stop reason: {}",
            result.stop_reason
        );
    }
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
                    .contains("x-github-api-version: 2026-06-01"));
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
            api.base_url = root.clone();
            let mut messages = AnthropicConfig::new("");
            messages.base_url = root;
            let client = CopilotClient {
                chat: OpenAiClient::new(chat).with_auth(auth.clone()),
                responses: OpenAiResponsesClient::new(api).with_auth(auth.clone()),
                messages: AnthropicClient::new(messages).with_auth(auth),
                default_model: "gpt-4.1".into(),
                catalog: None,
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

    /// Minimal Copilot stand-in: records `(path, body)` of every request and
    /// answers `/models` with `catalog` (or a 500) and completions with a tool call.
    struct MockCopilot {
        root: String,
        requests: Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>,
        _credentials: tempfile::TempDir,
        auth: Arc<CopilotAuth>,
    }
    impl MockCopilot {
        async fn start(catalog: Option<serde_json::Value>) -> Self {
            Self::start_rejecting(catalog, &[]).await
        }
        /// Like `start`, but completions for `rejected` models get Copilot's real
        /// `400 model_not_supported` answer.
        async fn start_rejecting(catalog: Option<serde_json::Value>, rejected: &[&str]) -> Self {
            let rejected: Vec<String> = rejected.iter().map(|m| m.to_string()).collect();
            use tokio::{
                io::{AsyncReadExt, AsyncWriteExt},
                net::TcpListener,
            };
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let root = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = requests.clone();
            tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let (log, catalog, rejected) = (log.clone(), catalog.clone(), rejected.clone());
                    tokio::spawn(async move {
                        let mut bytes = Vec::new();
                        let (path, body) = loop {
                            let mut chunk = [0; 4096];
                            let size = socket.read(&mut chunk).await.unwrap();
                            if size == 0 {
                                return;
                            }
                            bytes.extend_from_slice(&chunk[..size]);
                            let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                                continue;
                            };
                            let head = String::from_utf8_lossy(&bytes[..end]).to_string();
                            assert!(head
                                .to_lowercase()
                                .contains("x-github-api-version: 2026-06-01"));
                            // Every protocol and discovery must route custom
                            // OAuth credentials to the developer integration.
                            assert!(head
                                .to_lowercase()
                                .contains("copilot-integration-id: copilot-developer-cli"));
                            assert!(head
                                .to_lowercase()
                                .contains("authorization: bearer fixture"));
                            assert!(!head.to_lowercase().contains("x-api-key:"));
                            let length: usize = head
                                .lines()
                                .find_map(|line| {
                                    let (key, value) = line.split_once(':')?;
                                    key.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse().unwrap())
                                })
                                .unwrap_or(0);
                            if bytes.len() >= end + 4 + length {
                                let path = head.split_whitespace().nth(1).unwrap().to_owned();
                                let body =
                                    serde_json::from_slice(&bytes[end + 4..end + 4 + length])
                                        .unwrap_or(serde_json::Value::Null);
                                break (path, body);
                            }
                        };
                        let refused = body["model"]
                            .as_str()
                            .is_some_and(|m| rejected.iter().any(|r| r == m));
                        log.lock().unwrap().push((path.clone(), body));
                        let (status, content_type, payload) = match (path.as_str(), catalog) {
                            _ if refused => ("400 Bad Request", "application/json", r#"{"error":{"message":"The requested model is not supported.","code":"model_not_supported","param":"model","type":"invalid_request_error"}}"#.to_string()),
                            ("/models", Some(catalog)) => ("200 OK", "application/json", catalog.to_string()),
                            ("/models", None) => ("500 Internal Server Error", "application/json", "{}".into()),
                            ("/v1/messages", _) => {
                                let frames = [
                                    serde_json::json!({"type":"message_start","message":{"id":"msg_fixture","type":"message","role":"assistant","content":[],"model":"claude-haiku-4.5","usage":{"input_tokens":10,"output_tokens":0}}}),
                                    serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_fixture","name":"run_command","input":{}}}),
                                    serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"program\":\"npm\"}"}}),
                                    serde_json::json!({"type":"content_block_stop","index":0}),
                                    serde_json::json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
                                    serde_json::json!({"type":"message_stop"}),
                                ];
                                let sse = frames.iter().map(|frame| format!("event: {}\ndata: {frame}\n\n", frame["type"].as_str().unwrap())).collect::<String>();
                                ("200 OK", "text/event-stream", sse)
                            },
                            _ => {
                                let frames = [
                                    serde_json::json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_fixture","type":"function","function":{"name":"run_command","arguments":"{}"}}]},"finish_reason":null}]}),
                                    serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
                                ];
                                let mut sse: String = frames.iter().map(|f| format!("data: {f}\n\n")).collect();
                                sse.push_str("data: [DONE]\n\n");
                                ("200 OK", "text/event-stream", sse)
                            }
                        };
                        let response = format!("HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}", payload.len());
                        let _ = socket.write_all(response.as_bytes()).await;
                    });
                }
            });
            let credentials = tempfile::tempdir().unwrap();
            let path = credentials.path().join("config.json");
            std::fs::write(
                &path,
                serde_json::json!({"format":crate::credentials::CREDENTIAL_FORMAT,"host":"github.com","login":"test","oauth_token":"fixture"})
                    .to_string(),
            )
            .unwrap();
            let auth = Arc::new(CopilotAuth::new(Some(path), "github.com".into()));
            Self {
                root,
                requests,
                _credentials: credentials,
                auth,
            }
        }
        fn client(&self) -> CopilotClient {
            let mut chat = OpenAiConfig::new("");
            chat.base_url = self.root.clone();
            let mut responses = OpenAiResponsesConfig::new("");
            responses.base_url = self.root.clone();
            let mut messages = AnthropicConfig::new("");
            messages.base_url = self.root.clone();
            CopilotClient {
                chat: OpenAiClient::new(chat).with_auth(self.auth.clone()),
                responses: OpenAiResponsesClient::new(responses).with_auth(self.auth.clone()),
                messages: AnthropicClient::new(messages).with_auth(self.auth.clone()),
                default_model: "gpt-4.1".into(),
                catalog: Some(CatalogSource::new(&self.root, self.auth.clone())),
            }
        }
        /// Models named in completion requests, in order.
        fn completion_models(&self) -> Vec<String> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(path, _)| path != "/models")
                .map(|(_, body)| body["model"].as_str().unwrap_or_default().to_owned())
                .collect()
        }
        fn catalog_fetches(&self) -> usize {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(path, _)| path == "/models")
                .count()
        }
    }
    async fn ask(client: &CopilotClient, model: Option<&str>) -> Result<ModelResult, ModelError> {
        ask_with_events(client, model)
            .await
            .map(|(result, _)| result)
    }
    async fn ask_with_events(
        client: &CopilotClient,
        model: Option<&str>,
    ) -> Result<(ModelResult, Vec<harness_model::ModelEvent>), ModelError> {
        use harness_protocol::{
            ids::{MessageId, Timestamp},
            messages::{AgentMessage, ContentBlock, MessageRole},
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
            model: model.map(Into::into),
            max_tokens: Some(64),
            temperature: None,
            stop_sequences: vec![],
            extended_thinking: false,
            reasoning_effort: None,
            response_format: None,
            provider_options: serde_json::Value::Null,
        };
        let (events, mut receiver) = tokio::sync::mpsc::channel(32);
        let capture = tokio::spawn(async move {
            let mut captured = Vec::new();
            while let Some(event) = receiver.recv().await {
                captured.push(event);
            }
            captured
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            client.stream(request, events, tokio_util::sync::CancellationToken::new()),
        )
        .await
        .unwrap()?;
        Ok((result, capture.await.unwrap()))
    }
    #[tokio::test]
    async fn haiku_uses_native_messages_with_the_same_oauth_credential_as_discovery() {
        let mock = MockCopilot::start(Some(serde_json::json!({"data":[{
            "id":"claude-haiku-4.5", "supported_endpoints":["/chat/completions","/v1/messages"],
            "capabilities":{"limits":{"max_output_tokens":32}}
        }]})))
        .await;
        let (result, events) = ask_with_events(&mock.client(), Some("claude-haiku-4.5"))
            .await
            .unwrap();
        assert_eq!(result.stop_reason, "tool_use");
        assert!(events.iter().any(|event| matches!(event, harness_model::ModelEvent::ToolCallCompleted { name, input, .. } if name == "run_command" && input["program"] == "npm")));
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests[0].0, "/models");
        assert_eq!(requests[1].0, "/v1/messages");
        assert_eq!(requests[1].1["max_tokens"], 32);
        assert_eq!(requests[1].1["messages"][0]["role"], "user");
    }
    fn account_without_gpt_4_1() -> serde_json::Value {
        serde_json::json!({"data": [
            {"id": "gpt-4.1", "policy": {"state": "disabled"}, "model_picker_enabled": true},
            {"id": "claude-sonnet-4.5", "capabilities": {"type": "chat"}, "model_picker_enabled": true, "is_chat_default": true, "supported_endpoints": ["/chat/completions"]},
            {"id": "gemini-2.5-pro", "capabilities": {"type": "chat"}, "model_picker_enabled": true}
        ]})
    }

    #[tokio::test]
    async fn auto_uses_a_model_the_account_can_call_instead_of_a_hardcoded_default() {
        let mock = MockCopilot::start(Some(account_without_gpt_4_1())).await;
        let client = mock.client();
        // Both spellings of "auto": the UI's literal id and no model at all.
        ask(&client, Some("auto")).await.unwrap();
        ask(&client, None).await.unwrap();
        assert_eq!(
            mock.completion_models(),
            ["claude-sonnet-4.5", "claude-sonnet-4.5"]
        );
        assert_eq!(
            mock.catalog_fetches(),
            1,
            "catalog is cached between requests"
        );
    }

    #[tokio::test]
    async fn auto_keeps_the_configured_default_when_the_account_has_it() {
        let mut catalog = account_without_gpt_4_1();
        catalog["data"][0] = serde_json::json!({"id": "gpt-4.1", "model_picker_enabled": true});
        let mock = MockCopilot::start(Some(catalog)).await;
        ask(&mock.client(), Some("auto")).await.unwrap();
        assert_eq!(mock.completion_models(), ["gpt-4.1"]);
    }

    #[tokio::test]
    async fn explicit_model_missing_from_the_catalog_is_still_sent_to_copilot() {
        // Regression: a catalog that does not list the chosen model (new model,
        // different response shape) must never be the thing that rejects it.
        let mock = MockCopilot::start(Some(account_without_gpt_4_1())).await;
        ask(&mock.client(), Some("claude-sonnet-5")).await.unwrap();
        assert_eq!(mock.completion_models(), ["claude-sonnet-5"]);
    }

    #[tokio::test]
    async fn copilots_own_rejection_is_kept_and_annotated_with_listed_models() {
        let mock = MockCopilot::start_rejecting(Some(account_without_gpt_4_1()), &["gpt-4o"]).await;
        let error = ask(&mock.client(), Some("gpt-4o")).await.unwrap_err();
        let ModelError::BackendError { message, .. } = error else {
            panic!("expected a backend error");
        };
        assert_eq!(
            mock.completion_models(),
            ["gpt-4o"],
            "Copilot, not the catalog, made the call"
        );
        assert!(
            message.contains("\"gpt-4o\"") && message.contains("model_not_supported"),
            "{message}"
        );
        assert!(message.contains("sign in again in Rusty's settings"));
        assert!(
            message.contains("claude-sonnet-4.5") && message.contains("gemini-2.5-pro"),
            "{message}"
        );
        assert!(
            !message.contains("gpt-4.1,") && !message.contains(", gpt-4.1"),
            "disabled models are not suggested: {message}"
        );
    }

    #[tokio::test]
    async fn rejection_without_a_usable_catalog_never_prints_an_empty_list() {
        let mock = MockCopilot::start_rejecting(None, &["gpt-4o"]).await;
        let error = ask(&mock.client(), Some("gpt-4o")).await.unwrap_err();
        let ModelError::BackendError { message, .. } = error else {
            panic!("expected a backend error");
        };
        assert!(
            message.contains("model_not_supported") && !message.contains("lists:"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn catalog_flagging_no_model_for_the_picker_still_resolves_auto() {
        let mock = MockCopilot::start(Some(serde_json::json!({"data": [
            {"id": "gpt-4.1", "policy": {"state": "disabled"}, "model_picker_enabled": false},
            {"id": "claude-sonnet-4.5", "model_picker_enabled": false}
        ]})))
        .await;
        ask(&mock.client(), Some("auto")).await.unwrap();
        assert_eq!(mock.completion_models(), ["claude-sonnet-4.5"]);
    }

    #[tokio::test]
    async fn explicit_model_the_account_has_is_sent_as_chosen() {
        let mock = MockCopilot::start(Some(account_without_gpt_4_1())).await;
        ask(&mock.client(), Some("gemini-2.5-pro")).await.unwrap();
        assert_eq!(mock.completion_models(), ["gemini-2.5-pro"]);
    }

    #[tokio::test]
    async fn an_unreadable_catalog_never_blocks_inference() {
        let mock = MockCopilot::start(None).await;
        let client = mock.client();
        ask(&client, Some("auto")).await.unwrap();
        ask(&client, Some("some-new-model")).await.unwrap();
        assert_eq!(mock.completion_models(), ["gpt-4.1", "some-new-model"]);
    }

    #[test]
    fn catalog_routes_use_opencodes_native_endpoint_precedence() {
        let catalog = Catalog::parse(&serde_json::json!({"data": [
            {"id": "gpt-5.1-codex", "supported_endpoints": ["/responses"]},
            {"id": "gpt-5.2", "supported_endpoints": ["/chat/completions"]},
            {"id": "gpt-5.3", "supported_endpoints": ["/chat/completions", "/responses"]},
            {"id": "gpt-4.1", "supported_endpoints": ["/responses"]},
            {"id": "claude-sonnet-4.5", "supported_endpoints": ["/v1/messages"]},
            {"id": "gpt-5.4"}
        ]}))
        .unwrap();
        for (model, route) in [
            ("gpt-5.1-codex", InferenceRoute::Responses),
            ("gpt-5.2", InferenceRoute::Chat),
            ("gpt-5.3", InferenceRoute::Responses),
            ("gpt-4.1", InferenceRoute::Responses),
            ("claude-sonnet-4.5", InferenceRoute::Messages),
            ("gpt-5.4", InferenceRoute::Responses),
            ("not-in-catalog", InferenceRoute::Chat),
        ] {
            assert_eq!(inference_route(model, Some(&catalog)), route, "{model}");
        }
        assert_eq!(
            inference_route("gpt-5.2", None),
            InferenceRoute::Responses,
            "no catalog keeps name-based routing"
        );
    }
}
