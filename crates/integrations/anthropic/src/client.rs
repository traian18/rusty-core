//! Anthropic Messages API client implementing [`ModelClient`].
//!
//! [`ModelClient`]: harness_model::client::ModelClient

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tracing::instrument;

use harness_model::client::ModelClient;
use harness_model::events::{ModelError, ModelEvent, ModelResult};
use harness_model::request::{ModelCapabilities, ModelRequest};

use crate::config::AnthropicConfig;
use crate::wire::{
    build_system, convert_messages_with_tool_ids, tool_descriptor_to_anthropic,
    AnthropicRequest, AnthropicSseParser, AnthropicThinking, ProviderToolIds,
};

/// Client for the Anthropic Messages API.
///
/// Implements [`ModelClient`] by converting [`ModelRequest`] to the Anthropic
/// wire format, sending HTTP POST requests to the Anthropic API, and parsing
/// the Server-Sent Events (SSE) response stream into [`ModelEvent`]s.
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
    /// The underlying `reqwest::Client` is built with the request timeout
    /// specified in `config.request_timeout`. Construction panics only if
    /// `reqwest::Client::builder()` fails, which should never happen with
    /// the default builder settings used here.
    pub fn new(config: AnthropicConfig) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(config.request_timeout)
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
    ///    [`AnthropicSseParser`] and forwards each parsed [`ModelEvent`]
    ///    through the `events` broadcast channel.
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
    #[instrument(skip(self, request, events, cancel))]
    async fn stream(
        &self,
        request: ModelRequest,
        events: tokio::sync::broadcast::Sender<ModelEvent>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ModelResult, ModelError> {
        // ------------------------------------------------------------------
        // Step 1: Convert ModelRequest to AnthropicRequest
        // ------------------------------------------------------------------
        let max_tokens = request
            .max_tokens
            .unwrap_or(self.config.default_max_tokens);

        let anthropic_request = AnthropicRequest {
            model: request
                .model
                .unwrap_or_else(|| self.config.default_model.clone()),
            system: if !request.system_prompt.is_empty() {
                Some(request.system_prompt)
            } else {
                build_system(&request.messages)
            },
            messages: {
                let tool_ids = self
                    .tool_ids
                    .lock()
                    .expect("provider tool-id map poisoned");
                convert_messages_with_tool_ids(&request.messages, &tool_ids)
            },
            tools: if request.tools.is_empty() {
                None
            } else {
                Some(
                    request
                        .tools
                        .iter()
                        .map(tool_descriptor_to_anthropic)
                        .collect(),
                )
            },
            max_tokens,
            temperature: request.temperature,
            stop_sequences: if request.stop_sequences.is_empty() {
                None
            } else {
                Some(request.stop_sequences)
            },
            thinking: if request.extended_thinking {
                if max_tokens < 2048 {
                    return Err(ModelError::InvalidRequest {
                        message: "extended thinking requires max_tokens >= 2048".to_string(),
                    });
                }
                Some(AnthropicThinking {
                    kind: "enabled".to_string(),
                    budget_tokens: max_tokens - 1024,
                })
            } else {
                None
            },
        };

        // ------------------------------------------------------------------
        // Step 2: Send HTTP POST request
        // ------------------------------------------------------------------
        let url = format!("{}/v1/messages", self.config.base_url);

        let response = self
            .http_client
            .post(&url)
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&anthropic_request)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    ModelError::Timeout
                } else {
                    ModelError::BackendError {
                        message: format!("HTTP request failed: {error}"),
                        code: String::from("request_failed"),
                    }
                }
            })?;

        // ------------------------------------------------------------------
        // Step 3: Handle response status
        // ------------------------------------------------------------------
        let status = response.status();

        if status.is_success() {
            Self::handle_success_response(
                response,
                &events,
                &cancel,
                self.tool_ids.clone(),
            )
            .await
        } else if status.as_u16() == 429 {
            Self::handle_rate_limit(response)
        } else {
            Self::handle_error_response(response, status).await
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
        events: &tokio::sync::broadcast::Sender<ModelEvent>,
        cancel: &tokio_util::sync::CancellationToken,
        tool_ids: ProviderToolIds,
    ) -> Result<ModelResult, ModelError> {
        let mut parser = AnthropicSseParser::with_tool_ids(tool_ids);

        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return Err(ModelError::Cancelled),
                chunk = response.chunk() => chunk.map_err(|error| {
                    if error.is_timeout() {
                        ModelError::Timeout
                    } else {
                        ModelError::Protocol {
                            message: format!("failed to read Anthropic SSE stream: {error}"),
                        }
                    }
                })?,
            };
            let Some(chunk) = chunk else {
                break;
            };

            for event in parser.push_chunk(&chunk)? {
                if cancel.is_cancelled() {
                    return Err(ModelError::Cancelled);
                }
                let _ = events.send(event);
            }
        }

        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        let (terminal_events, result) = parser.finish()?;
        for event in terminal_events {
            let _ = events.send(event);
        }
        Ok(result)
    }

    /// Handle an HTTP 429 rate-limit response.
    fn handle_rate_limit(
        response: reqwest::Response,
    ) -> Result<ModelResult, ModelError> {
        let retry_after = retry_after(response.headers());

        Err(ModelError::RateLimited { retry_after })
    }

    /// Handle a non-success, non-429 HTTP response.
    async fn handle_error_response(
        response: reqwest::Response,
        status: reqwest::StatusCode,
    ) -> Result<ModelResult, ModelError> {
        let body = response.text().await.unwrap_or_default();
        Err(ModelError::BackendError {
            message: format!("HTTP {status}: {body}"),
            code: status.as_u16().to_string(),
        })
    }
}

/// Normalize standard `Retry-After` (seconds) and Anthropic's millisecond hint.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .or_else(|| {
            headers
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(Duration::from_secs)
        })
}

#[cfg(test)]
mod tests {
    use super::retry_after;
    use std::time::Duration;

    #[test]
    fn retry_after_ms_is_normalized_to_duration() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after-ms", "1250".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_millis(1250)));
    }

    #[test]
    fn standard_retry_after_uses_seconds() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "3".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(3)));
    }
}
