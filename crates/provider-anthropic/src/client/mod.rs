//! Anthropic Messages API client implementing [`ModelClient`].
//!
//! `request` builds the body and makes the call; `response` reads what comes back.
//!
//! [`ModelClient`]: rusty_model_backend::client::ModelClient

mod request;
mod response;

use response::parse_max_tokens_ceiling;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tracing::instrument;

use rusty_model_backend::client::{ModelClient, ModelEventSender};
use rusty_model_backend::events::{ModelError, ModelResult};
use rusty_model_backend::request::{ModelCapabilities, ModelRequest};

use crate::config::AnthropicConfig;
use crate::wire::{is_adaptive_thinking_rejection, ProviderToolIds};

/// Client for the Anthropic Messages API.
///
/// Implements [`ModelClient`] by converting [`ModelRequest`] to the Anthropic
/// wire format, sending HTTP POST requests to the Anthropic API, and parsing
/// the Server-Sent Events (SSE) response stream into [`ModelEvent`](rusty_model_backend::events::ModelEvent)s.
///
/// The client is constructed with an [`AnthropicConfig`] that controls the
/// API key, base URL, default model, timeout, and other settings.
///
/// # Capabilities
///
/// The client advertises support for streaming, reasoning / extended thinking,
/// tool calls, and parallel tool calls.
pub struct AnthropicClient {
    /// Configuration for the Anthropic API.
    config: AnthropicConfig,
    /// Reusable HTTP client built with the configured request timeout.
    http_client: reqwest::Client,
    /// Provider-issued tool IDs retained across model turns.
    tool_ids: ProviderToolIds,
    /// Models a provider has rejected budget (`"enabled"`) thinking for, so
    /// later requests go straight to adaptive thinking instead of paying for
    /// the rejected round trip again.
    adaptive_thinking_models: Arc<Mutex<HashSet<String>>>,
    auth: Option<Arc<dyn rusty_model_backend::auth::InferenceAuth>>,
}

impl AnthropicClient {
    /// Create a new [`AnthropicClient`] from the given configuration.
    ///
    /// The underlying `reqwest::Client` is built with the HTTP read inactivity timeout
    /// specified in `config.request_timeout`. Construction panics only if
    /// `reqwest::Client::builder()` fails, which should never happen with
    /// the default builder settings used here.
    pub fn new(config: AnthropicConfig) -> Self {
        let http_client = reqwest::Client::builder()
            .read_timeout(config.request_timeout)
            .build()
            .expect("reqwest::ClientBuilder::build should not fail with default settings");
        Self {
            config,
            http_client,
            tool_ids: Arc::new(Mutex::new(HashMap::new())),
            adaptive_thinking_models: Arc::new(Mutex::new(HashSet::new())),
            auth: None,
        }
    }
    pub fn with_auth(mut self, auth: Arc<dyn rusty_model_backend::auth::InferenceAuth>) -> Self {
        self.auth = Some(auth);
        self
    }
}

#[async_trait]
impl ModelClient for AnthropicClient {
    /// Returns the capabilities supported by this Anthropic client.
    ///
    /// All capabilities — streaming, reasoning (extended thinking),
    /// tool calls, and parallel tool calls — are supported.
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            streaming: true,
            reasoning: true,
            tool_calls: true,
            parallel_tool_calls: true,
            images: true,
            structured_output: true,
        }
    }

    /// Execute a streaming request against the Anthropic Messages API.
    ///
    /// This method:
    /// 1. Converts the [`ModelRequest`] into an [`AnthropicRequest`](crate::wire::AnthropicRequest) using
    ///    the conversion functions from [`wire`](crate::wire).
    /// 2. Sends an HTTP POST to `{config.base_url}/v1/messages` with the
    ///    required Anthropic headers (`x-api-key`, `anthropic-version`,
    ///    `content-type`).
    /// 3. On success (HTTP 2xx), parses the SSE response body using
    ///    [`AnthropicSseParser`](crate::wire::AnthropicSseParser) and forwards each parsed [`ModelEvent`](rusty_model_backend::events::ModelEvent)
    ///    through the bounded `events` channel.
    /// 4. On HTTP 429, reads the `retry-after-ms` header and returns
    ///    [`ModelError::RateLimited`].
    /// 5. On other HTTP errors, returns [`ModelError::BackendError`] with
    ///    the status code and response body.
    ///
    /// # Cancellation
    ///
    /// The `cancel` token is checked before forwarding each event. When
    /// cancellation is signalled the method returns [`ModelError::Cancelled`]
    /// immediately.
    ///
    /// # Timeouts
    ///
    /// Both connection-level timeouts (raised by `reqwest` while sending the
    /// request) and stream-read timeouts (raised while awaiting the next SSE
    /// chunk) are classified as [`ModelError::Timeout`] rather than a generic
    /// [`ModelError::BackendError`], so the recovery policy can treat them as
    /// retryable transient failures.
    #[instrument(skip(self, request, events, cancel))]
    async fn stream(
        &self,
        request: ModelRequest,
        events: ModelEventSender,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ModelResult, ModelError> {
        let invalid_names = crate::wire::find_invalid_tool_names(&request.tools);
        if !invalid_names.is_empty() {
            return Err(ModelError::InvalidRequest {
                message: format!(
                    "tool name(s) incompatible with the Anthropic Messages API \
                     (must match ^[a-zA-Z0-9_-]{{1,128}}$): {}",
                    invalid_names.join(", ")
                ),
            });
        }

        let requested_max_tokens = request.max_tokens.unwrap_or(self.config.default_max_tokens);
        let url = format!("{}/v1/messages", self.config.base_url);
        let model = self.model_for(&request);
        let mut max_tokens = requested_max_tokens;
        let mut adaptive = self.uses_adaptive_thinking(&model);
        let mut corrected_max_tokens = false;
        let wants_thinking = request.extended_thinking || request.reasoning_effort.is_some();

        loop {
            let body = self.build_body(&request, max_tokens, adaptive)?;
            let response = tokio::select! {
                _ = cancel.cancelled() => return Err(ModelError::Cancelled),
                result = self.send_messages(&url, &body) => result?,
            };
            let status = response.status();

            if status.is_success() {
                return Self::handle_success_response(
                    response,
                    &events,
                    &cancel,
                    self.tool_ids.clone(),
                )
                .await;
            }
            if status.as_u16() == 429 {
                return Self::handle_rate_limit(response);
            }
            if status.as_u16() != 400 {
                return Self::handle_error_response(response, status).await;
            }
            let error_body = response.text().await.unwrap_or_default();

            // Newer models (Claude Opus 4.7 onward) reject budget thinking
            // outright and accept only adaptive thinking. Model ids differ by
            // gateway (Copilot spells them `claude-opus-4.7`) and new ones
            // keep shipping, so `requires_adaptive_thinking` can't be the only
            // guard: switch once on the provider's own rejection, and remember
            // the model so the rest of the session skips the failed attempt.
            if wants_thinking && !adaptive && is_adaptive_thinking_rejection(&error_body) {
                tracing::warn!(
                    model,
                    "provider rejected budget thinking for this model; retrying with adaptive thinking"
                );
                self.adaptive_thinking_models
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(model.clone());
                adaptive = true;
                continue;
            }

            // A model's real per-model output-token ceiling isn't reliably known
            // ahead of time by every caller (it varies per model and changes as
            // new ones ship), so a too-high `max_tokens` reaches here as a
            // provider-side 400 instead of being pre-validated locally. Anthropic
            // reports it in a stable, parseable shape: "max_tokens: <requested> >
            // <allowed>, which is the maximum allowed number of output tokens for
            // <model>" -- also seen relayed verbatim through a gateway's own
            // "Upstream request failed: ..." wrapper (OpenCode Zen's `anthropic`
            // route does this). Rather than fail a request the model could have
            // answered fine at its real limit, self-correct once and retry with
            // that limit instead of the guessed one -- this is also why the
            // extended-thinking budget (itself a function of `max_tokens`, see
            // `resolve_thinking`) is re-derived via `build_body` rather than the
            // rejected request simply being replayed with one field patched.
            if !corrected_max_tokens {
                if let Some(allowed) =
                    parse_max_tokens_ceiling(&error_body).filter(|&allowed| allowed < max_tokens)
                {
                    tracing::warn!(
                        requested = max_tokens,
                        allowed,
                        "provider rejected max_tokens above the model's real output ceiling; retrying once with the corrected value"
                    );
                    corrected_max_tokens = true;
                    max_tokens = allowed;
                    continue;
                }
            }
            return Self::handle_error_response_from_body(status, error_body);
        }
    }
}

#[cfg(test)]
mod tests;
