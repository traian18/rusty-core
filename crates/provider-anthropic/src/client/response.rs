//! Reading the response: success, rate limits and errors.

use std::time::Duration;

use rusty_model_backend::client::{send_event, ModelEventSender};
use rusty_model_backend::events::{ModelError, ModelResult};

use crate::wire::{AnthropicSseParser, ProviderToolIds};

use super::AnthropicClient;

impl AnthropicClient {
    /// Incrementally parse a successful SSE response and forward events.
    pub(super) async fn handle_success_response(
        mut response: reqwest::Response,
        events: &ModelEventSender,
        cancel: &tokio_util::sync::CancellationToken,
        tool_ids: ProviderToolIds,
    ) -> Result<ModelResult, ModelError> {
        let is_json = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|ct| ct.contains("application/json"));

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
    pub(super) fn handle_rate_limit(
        response: reqwest::Response,
    ) -> Result<ModelResult, ModelError> {
        let retry_after = retry_after_from_headers(response.headers());

        Err(ModelError::RateLimited { retry_after })
    }

    /// Handle a non-success, non-429 HTTP response.
    pub(super) async fn handle_error_response(
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
    pub(super) fn handle_error_response_from_body(
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
pub(super) fn retry_after_from_headers(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    rusty_model_backend::retry::parse_retry_after(|name| {
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
pub(super) fn parse_max_tokens_ceiling(body: &str) -> Option<u64> {
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
