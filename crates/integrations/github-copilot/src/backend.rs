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
mod tests;
