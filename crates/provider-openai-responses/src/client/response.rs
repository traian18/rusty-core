//! Reading the response: the stream on success, and the right error otherwise.

use rusty_model_backend::client::{send_event, ModelEventSender};
use rusty_model_backend::events::{ModelError, ModelResult};

use super::OpenAiResponsesClient;
use crate::wire::{OpenAiResponsesSseParser, ProviderToolIds};

impl OpenAiResponsesClient {
    /// Reads the HTTP response: streams a success, and turns anything else into the right error.
    pub(super) async fn read_response(
        &self,
        response: reqwest::Response,
        events: &ModelEventSender,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<ModelResult, ModelError> {
        let status = response.status();
        if status.is_success() {
            Self::handle_success_response(
                response,
                events,
                cancel,
                self.tool_ids.clone(),
                self.chatgpt_subscription,
            )
            .await
        } else if status.as_u16() == 429 {
            Self::handle_rate_limit(response).await
        } else {
            Self::handle_error_response(response, status).await
        }
    }

    pub(super) async fn handle_success_response(
        mut response: reqwest::Response,
        events: &ModelEventSender,
        cancel: &tokio_util::sync::CancellationToken,
        tool_ids: ProviderToolIds,
        subscription: bool,
    ) -> Result<ModelResult, ModelError> {
        let mut parser = OpenAiResponsesSseParser::with_tool_ids(tool_ids);

        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return Err(ModelError::Cancelled),
                chunk = response.chunk() => chunk.map_err(|error| {
                    if error.is_timeout() {
                        ModelError::Timeout
                    } else {
                        ModelError::StreamInterrupted {
                            message: format!("failed to read Responses SSE stream: {error}"),
                        }
                    }
                })?,
            };
            let Some(chunk) = chunk else { break };

            for mut event in parser.push_chunk(&chunk)? {
                if subscription {
                    if let rusty_model_backend::ModelEvent::Completed { result } = &mut event {
                        result.cost = Default::default();
                    }
                }
                send_event(events, event, cancel).await?;
            }
        }

        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        let (terminal_events, mut result) = parser.finish()?;
        if subscription {
            result.cost = Default::default();
        }
        for event in terminal_events {
            send_event(events, event, cancel).await?;
        }
        Ok(result)
    }

    pub(super) async fn handle_rate_limit(
        response: reqwest::Response,
    ) -> Result<ModelResult, ModelError> {
        let from_headers = retry_after_from_headers(response.headers());
        let body = response.text().await.unwrap_or_default();
        let retry_after = from_headers.or_else(|| usage_limit_reset(&body, now_unix()));
        Err(ModelError::RateLimited { retry_after })
    }

    pub(super) async fn handle_error_response(
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

/// When a ChatGPT subscription's usage limit resets, from the 429 body
/// (`{"error": {"type": "usage_limit_reached", "resets_in_seconds": …}}`, or
/// `resets_at` in Unix seconds), so the run can say when to continue.
pub(super) fn usage_limit_reset(body: &str, now: u64) -> Option<std::time::Duration> {
    let error = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let error = error.get("error")?;
    if let Some(seconds) = error.get("resets_in_seconds").and_then(|v| v.as_u64()) {
        return Some(std::time::Duration::from_secs(seconds));
    }
    let at = error.get("resets_at").and_then(|v| v.as_u64())?;
    Some(std::time::Duration::from_secs(at.saturating_sub(now)))
}

pub(super) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

pub(super) fn retry_after_from_headers(
    headers: &reqwest::header::HeaderMap,
) -> Option<std::time::Duration> {
    rusty_model_backend::retry::parse_retry_after(|name| {
        headers.get(name).and_then(|value| value.to_str().ok())
    })
}
