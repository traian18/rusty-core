//! Anthropic Messages API client implementing [`ModelClient`].
//!
//! [`ModelClient`]: harness_model::client::ModelClient

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tracing::instrument;

use harness_model::client::{send_event, ModelClient, ModelEventSender};
use harness_model::events::{ModelError, ModelResult};
use harness_model::request::{ModelCapabilities, ModelRequest};

use crate::config::AnthropicConfig;
use crate::wire::{
    build_system, convert_messages_with_tool_ids, resolve_thinking, tool_descriptor_to_anthropic,
    AnthropicRequest, AnthropicSseParser, ProviderToolIds,
};

/// Client for the Anthropic Messages API.
///
/// Implements [`ModelClient`] by converting [`ModelRequest`] to the Anthropic
/// wire format, sending HTTP POST requests to the Anthropic API, and parsing
/// the Server-Sent Events (SSE) response stream into [`ModelEvent`](harness_model::events::ModelEvent)s.
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
        }
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
    /// 1. Converts the [`ModelRequest`] into an [`AnthropicRequest`] using
    ///    the conversion functions from [`wire`](crate::wire).
    /// 2. Sends an HTTP POST to `{config.base_url}/v1/messages` with the
    ///    required Anthropic headers (`x-api-key`, `anthropic-version`,
    ///    `content-type`).
    /// 3. On success (HTTP 2xx), parses the SSE response body using
    ///    [`AnthropicSseParser`] and forwards each parsed [`ModelEvent`](harness_model::events::ModelEvent)
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
        let body = self.build_body(&request, requested_max_tokens)?;
        let response = self
            .post_messages(&url, &body)
            .send()
            .await
            .map_err(Self::map_send_error)?;
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
        let error_body = response.text().await.unwrap_or_default();
        let Some(allowed) =
            parse_max_tokens_ceiling(&error_body).filter(|&allowed| allowed < requested_max_tokens)
        else {
            return Self::handle_error_response_from_body(status, error_body);
        };
        tracing::warn!(
            requested = requested_max_tokens,
            allowed,
            "provider rejected max_tokens above the model's real output ceiling; retrying once with the corrected value"
        );
        let corrected_body = self.build_body(&request, allowed)?;
        let retry_response = self
            .post_messages(&url, &corrected_body)
            .send()
            .await
            .map_err(Self::map_send_error)?;
        let retry_status = retry_response.status();

        if retry_status.is_success() {
            Self::handle_success_response(retry_response, &events, &cancel, self.tool_ids.clone())
                .await
        } else if retry_status.as_u16() == 429 {
            Self::handle_rate_limit(retry_response)
        } else {
            Self::handle_error_response(retry_response, retry_status).await
        }
    }
}

impl AnthropicClient {
    /// Builds the full merged JSON request body for a specific `max_tokens`.
    /// Extracted out of `stream()` so a request rejected for exceeding the
    /// model's real ceiling (see `stream()`'s 400 handling) can be rebuilt
    /// with the corrected value and resent -- including re-deriving the
    /// extended-thinking budget, which is itself a function of `max_tokens`
    /// and would otherwise still reference the rejected, too-high one.
    fn build_body(
        &self,
        request: &ModelRequest,
        max_tokens: u64,
    ) -> Result<serde_json::Value, ModelError> {
        // Anthropic has no `response_format`; a non-text format is emulated
        // with a forced single-purpose tool call. See `structured_output_tool`.
        let structured_output = request
            .response_format
            .as_ref()
            .and_then(crate::wire::structured_output_tool);

        let anthropic_request = AnthropicRequest {
            model: request
                .model
                .clone()
                .unwrap_or_else(|| self.config.default_model.clone()),
            system: if !request.system_prompt.is_empty() {
                Some(request.system_prompt.clone())
            } else {
                build_system(&request.messages)
            },
            messages: {
                let tool_ids = self.tool_ids.lock().expect("provider tool-id map poisoned");
                convert_messages_with_tool_ids(&request.messages, &tool_ids)
            },
            tools: {
                let mut tools: Vec<_> = request
                    .tools
                    .iter()
                    .map(tool_descriptor_to_anthropic)
                    .collect();
                // The synthetic structured-output tool rides alongside the
                // host's real tools; `tool_choice` below forces the model
                // onto it, so the others are unreachable for this request.
                if let Some((tool, _)) = structured_output.as_ref() {
                    tools.push(tool.clone());
                }
                (!tools.is_empty()).then_some(tools)
            },
            tool_choice: structured_output.map(|(_, choice)| choice),
            max_tokens,
            temperature: request.temperature,
            stop_sequences: if request.stop_sequences.is_empty() {
                None
            } else {
                Some(request.stop_sequences.clone())
            },
            thinking: resolve_thinking(
                request.extended_thinking,
                request.reasoning_effort,
                max_tokens,
            )?,
            stream: true,
        };

        // M4: merge caller-supplied `provider_options["anthropic"]` knobs
        // (e.g. `top_k`) that have no typed field on `AnthropicRequest` —
        // see `harness_model::merge_provider_options`'s doc comment for the
        // precedence rule (typed fields above always win).
        Ok(harness_model::merge_provider_options(
            serde_json::to_value(&anthropic_request).map_err(|error| {
                ModelError::InvalidRequest {
                    message: format!("failed to serialize request: {error}"),
                }
            })?,
            &request.provider_options,
            "anthropic",
        ))
    }

    /// Builds the POST request (URL + auth/content headers), ready for
    /// `.json(body).send()`. Shared by the initial attempt and the
    /// corrected-`max_tokens` retry in `stream()`.
    fn post_messages(&self, url: &str, body: &serde_json::Value) -> reqwest::RequestBuilder {
        let mut request_builder = self
            .http_client
            .post(url)
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json");

        if !self.config.api_key.is_empty() {
            request_builder =
                request_builder.header("authorization", format!("Bearer {}", self.config.api_key));
        }

        request_builder.json(body)
    }

    fn map_send_error(error: reqwest::Error) -> ModelError {
        if error.is_timeout() {
            ModelError::Timeout
        } else {
            ModelError::BackendError {
                message: format!("HTTP request failed: {error}"),
                code: String::from("request_failed"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

impl AnthropicClient {
    /// Incrementally parse a successful SSE response and forward events.
    async fn handle_success_response(
        mut response: reqwest::Response,
        events: &ModelEventSender,
        cancel: &tokio_util::sync::CancellationToken,
        tool_ids: ProviderToolIds,
    ) -> Result<ModelResult, ModelError> {
        let is_json = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map_or(false, |ct| ct.contains("application/json"));

        if is_json {
            let body = response
                .text()
                .await
                .map_err(|error| ModelError::Protocol {
                    message: format!("failed to read Anthropic JSON response: {error}"),
                })?;
            let mut parser = AnthropicSseParser::with_tool_ids(tool_ids);
            let _ = parser.push_chunk(body.as_bytes())?;
            let (terminal_events, result) = parser.finish()?;
            for event in terminal_events {
                send_event(events, event, cancel).await?;
            }
            return Ok(result);
        }

        let mut parser = AnthropicSseParser::with_tool_ids(tool_ids);

        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return Err(ModelError::Cancelled),
                chunk = response.chunk() => chunk.map_err(|error| {
                    if error.is_timeout() {
                        ModelError::Timeout
                    } else {
                        ModelError::StreamInterrupted {
                            message: format!("failed to read Anthropic SSE stream: {error}"),
                        }
                    }
                })?,
            };
            let Some(chunk) = chunk else {
                break;
            };

            for event in parser.push_chunk(&chunk)? {
                send_event(events, event, cancel).await?;
            }
        }

        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        let (terminal_events, result) = parser.finish()?;
        for event in terminal_events {
            send_event(events, event, cancel).await?;
        }
        Ok(result)
    }

    /// Handle an HTTP 429 rate-limit response.
    fn handle_rate_limit(response: reqwest::Response) -> Result<ModelResult, ModelError> {
        let retry_after = retry_after_from_headers(response.headers());

        Err(ModelError::RateLimited { retry_after })
    }

    /// Handle a non-success, non-429 HTTP response.
    async fn handle_error_response(
        response: reqwest::Response,
        status: reqwest::StatusCode,
    ) -> Result<ModelResult, ModelError> {
        let body = response.text().await.unwrap_or_default();
        Self::handle_error_response_from_body(status, body)
    }

    /// Same as [`Self::handle_error_response`], for a body already read out
    /// of its `Response` (a `Response` can only be read once) -- used by
    /// `stream()`'s 400 handling, which must inspect the body itself before
    /// deciding whether it's the max_tokens-ceiling case it can self-correct.
    fn handle_error_response_from_body(
        status: reqwest::StatusCode,
        body: String,
    ) -> Result<ModelResult, ModelError> {
        Err(ModelError::BackendError {
            message: format!("HTTP {status}: {body}"),
            code: status.as_u16().to_string(),
        })
    }
}

/// Normalize Anthropic's `retry-after-ms` and the standard `Retry-After`
/// (seconds) headers using the shared, provider-neutral parser.
fn retry_after_from_headers(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    harness_model::retry::parse_retry_after(|name| {
        headers.get(name).and_then(|value| value.to_str().ok())
    })
}

/// Extracts the real per-model output-token ceiling from an Anthropic
/// `invalid_request_error` body reporting `max_tokens: <requested> ><allowed>,
/// which is the maximum allowed number of output tokens for <model>` --
/// Anthropic's own message format, stable enough to parse without pulling in
/// a regex dependency for one substring pattern. Also matches the same text
/// relayed verbatim inside a gateway's own wrapper (e.g. OpenCode Zen's
/// `"Upstream request failed: [invalid_request_error] max_tokens: ..."`
/// envelope), since the substring itself is unchanged either way.
fn parse_max_tokens_ceiling(body: &str) -> Option<u64> {
    let after_marker = body.split("max_tokens:").nth(1)?;
    let allowed_segment = after_marker.split('>').nth(1)?;
    let digits: String = allowed_segment
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::{parse_max_tokens_ceiling, retry_after_from_headers, AnthropicClient};
    use crate::config::AnthropicConfig;
    use harness_model::request::ModelRequest;
    use harness_protocol::backend::ReasoningEffort;
    use std::time::Duration;

    #[test]
    fn retry_after_ms_is_normalized_to_duration() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after-ms", "1250".parse().unwrap());
        assert_eq!(
            retry_after_from_headers(&headers),
            Some(Duration::from_millis(1250))
        );
    }

    #[test]
    fn standard_retry_after_uses_seconds() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "3".parse().unwrap());
        assert_eq!(
            retry_after_from_headers(&headers),
            Some(Duration::from_secs(3))
        );
    }

    /// The exact body reported live against `claude-sonnet-4-5-20250929`.
    #[test]
    fn parses_the_ceiling_out_of_anthropics_own_error_format() {
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: 128000 > 64000, which is the maximum allowed number of output tokens for claude-sonnet-4-5-20250929"}}"#;
        assert_eq!(parse_max_tokens_ceiling(body), Some(64_000));
    }

    /// The same message, relayed inside a gateway's own wrapper -- OpenCode
    /// Zen's `anthropic`-family route reports it exactly this way.
    #[test]
    fn parses_the_ceiling_out_of_a_gateways_relayed_wrapper() {
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Upstream request failed: [invalid_request_error] max_tokens: 128000 > 64000, which is the maximum allowed number of output tokens for claude-sonnet-4-5-20250929"}}"#;
        assert_eq!(parse_max_tokens_ceiling(body), Some(64_000));
    }

    #[test]
    fn returns_none_for_an_unrelated_error() {
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        assert_eq!(parse_max_tokens_ceiling(body), None);
    }

    #[test]
    fn returns_none_for_malformed_text_after_the_marker() {
        assert_eq!(
            parse_max_tokens_ceiling("max_tokens: not a number here"),
            None
        );
        assert_eq!(
            parse_max_tokens_ceiling("max_tokens: 128000, no comparison"),
            None
        );
        assert_eq!(parse_max_tokens_ceiling(""), None);
    }

    fn client() -> AnthropicClient {
        AnthropicClient::new(AnthropicConfig::default())
    }

    fn request(max_tokens: u64, reasoning_effort: Option<ReasoningEffort>) -> ModelRequest {
        ModelRequest {
            system_prompt: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            model: Some("claude-sonnet-4-5-20250929".to_string()),
            max_tokens: Some(max_tokens),
            temperature: None,
            stop_sequences: Vec::new(),
            extended_thinking: false,
            reasoning_effort,
            response_format: None,
            provider_options: serde_json::Value::Null,
        }
    }

    /// The bug this whole mechanism exists to avoid: rebuilding the request
    /// with a corrected `max_tokens` must also correct `thinking.budget_tokens`
    /// (`resolve_thinking`'s own budget is a fraction of `max_tokens`), or a
    /// self-corrected retry could still be rejected -- this time for a
    /// thinking budget that no longer fits under the new, lower `max_tokens`.
    #[test]
    fn build_body_rederives_the_thinking_budget_for_the_corrected_max_tokens() {
        let client = client();
        let high_budget = client
            .build_body(&request(128_000, Some(ReasoningEffort::High)), 128_000)
            .expect("build_body succeeds");
        let corrected_budget = client
            .build_body(&request(128_000, Some(ReasoningEffort::High)), 64_000)
            .expect("build_body succeeds after correction");

        assert_eq!(high_budget["max_tokens"], 128_000);
        assert_eq!(corrected_budget["max_tokens"], 64_000);

        let high_tokens = high_budget["thinking"]["budget_tokens"]
            .as_u64()
            .expect("thinking budget must be present for a reasoning request");
        let corrected_tokens = corrected_budget["thinking"]["budget_tokens"]
            .as_u64()
            .expect("thinking budget must be present for a reasoning request");
        assert!(
            corrected_tokens < high_tokens,
            "budget must shrink with max_tokens, not stay pinned to the rejected value \
             (high={high_tokens}, corrected={corrected_tokens})"
        );
        assert!(
            corrected_tokens < 64_000,
            "budget must fit under the corrected max_tokens: {corrected_tokens}"
        );
    }

    #[test]
    fn build_body_sends_no_thinking_field_when_no_reasoning_was_requested() {
        let client = client();
        let body = client
            .build_body(&request(64_000, None), 64_000)
            .expect("build_body succeeds");
        assert!(body.get("thinking").is_none());
    }
}
