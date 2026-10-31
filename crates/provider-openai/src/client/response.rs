//! Reading the response: the stream on success, the right error otherwise, and OpenRouter's
//! usage lookup.

use rusty_model_backend::client::{send_event, ModelEventSender};
use rusty_model_backend::events::{ModelError, ModelResult};

use rusty_protocol::usage::{ModelUsage, UsageValue};

use crate::wire::{OpenAiSseParser, ProviderToolIds};

use super::OpenAiClient;

impl OpenAiClient {
    /// Reads the HTTP response: streams a success, and turns anything else into the right error.
    pub(super) async fn read_response(
        &self,
        response: reqwest::Response,
        events: &ModelEventSender,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<ModelResult, ModelError> {
        let status = response.status();
        if status.is_success() {
            self.handle_success_response(response, events, cancel, self.tool_ids.clone())
                .await
        } else if status.as_u16() == 429 {
            Self::handle_rate_limit(response)
        } else {
            Self::handle_error_response(response, status).await
        }
    }

    pub(super) async fn handle_success_response(
        &self,
        mut response: reqwest::Response,
        events: &ModelEventSender,
        cancel: &tokio_util::sync::CancellationToken,
        tool_ids: ProviderToolIds,
    ) -> Result<ModelResult, ModelError> {
        let mut parser = OpenAiSseParser::with_tool_ids(tool_ids);

        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return Err(ModelError::Cancelled),
                chunk = response.chunk() => chunk.map_err(|error| {
                    if error.is_timeout() {
                        ModelError::Timeout
                    } else {
                        ModelError::StreamInterrupted {
                            message: format!("failed to read OpenAI SSE stream: {error}"),
                        }
                    }
                })?,
            };
            let Some(chunk) = chunk else { break };

            for event in parser.push_chunk(&chunk)? {
                send_event(events, event, cancel).await?;
            }
        }

        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        // OpenRouter occasionally ends Gemini streams without the terminal
        // usage chunk. Its generation record is populated asynchronously and
        // can supply the exact counters before the core session completes.
        if parser.is_complete() && !parser.has_usage() && is_openrouter_url(&self.config.base_url) {
            if let Some(id) = parser.response_id() {
                if let Some(usage) = recover_openrouter_usage(
                    &self.http_client,
                    &self.config.base_url,
                    &self.config.api_key,
                    id,
                    cancel,
                )
                .await
                {
                    parser.set_usage(usage);
                }
            }
        }
        let (terminal_events, result) = parser.finish()?;
        for event in terminal_events {
            send_event(events, event, cancel).await?;
        }
        Ok(result)
    }

    pub(super) fn handle_rate_limit(
        response: reqwest::Response,
    ) -> Result<ModelResult, ModelError> {
        let retry_after = retry_after_from_headers(response.headers());
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

pub(super) fn is_openrouter_url(base_url: &str) -> bool {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| host == "openrouter.ai")
}

pub(super) fn generation_usage(data: &serde_json::Value) -> Option<ModelUsage> {
    let count = |native: &str, standard: &str| {
        let native_count = data.get(native).and_then(serde_json::Value::as_u64);
        let standard_count = data.get(standard).and_then(serde_json::Value::as_u64);
        match native_count {
            Some(0) => standard_count.or(native_count),
            _ => native_count.or(standard_count),
        }
    };
    let prompt = count("native_tokens_prompt", "tokens_prompt");
    let completion = count("native_tokens_completion", "tokens_completion");
    if prompt.unwrap_or(0) == 0 && completion.unwrap_or(0) == 0 {
        return None;
    }
    let cached = data
        .get("native_tokens_cached")
        .and_then(serde_json::Value::as_u64);
    let reasoning = data
        .get("native_tokens_reasoning")
        .and_then(serde_json::Value::as_u64);
    Some(ModelUsage {
        input_tokens: UsageValue::new(prompt),
        output_tokens: UsageValue::new(completion),
        cache_read_tokens: UsageValue::new(cached),
        cache_write_tokens: UsageValue::new(None),
        reasoning_tokens: UsageValue::new(reasoning),
        total_tokens: UsageValue::new(Some(prompt.unwrap_or(0) + completion.unwrap_or(0))),
    })
}

pub(super) async fn recover_openrouter_usage(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    response_id: &str,
    cancel: &tokio_util::sync::CancellationToken,
) -> Option<ModelUsage> {
    let mut url =
        reqwest::Url::parse(&format!("{}/generation", base_url.trim_end_matches('/'))).ok()?;
    url.query_pairs_mut().append_pair("id", response_id);
    for delay_ms in [0, 150, 350, 750, 1_500, 2_500, 4_000] {
        tokio::select! {
            _ = cancel.cancelled() => return None,
            _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
        }
        let request = async {
            let response = client
                .get(url.clone())
                .bearer_auth(api_key)
                .send()
                .await
                .ok()?;
            if !response.status().is_success() {
                return None;
            }
            let body = response.json::<serde_json::Value>().await.ok()?;
            body.get("data").and_then(generation_usage)
        };
        let result = tokio::select! {
            _ = cancel.cancelled() => return None,
            result = tokio::time::timeout(std::time::Duration::from_secs(2), request) => result,
        };
        if let Ok(Some(usage)) = result {
            return Some(usage);
        }
    }
    None
}

/// Normalize OpenAI's standard `Retry-After` header (whole seconds) using the
/// shared, provider-neutral parser.
pub(super) fn retry_after_from_headers(
    headers: &reqwest::header::HeaderMap,
) -> Option<std::time::Duration> {
    rusty_model_backend::retry::parse_retry_after(|name| {
        headers.get(name).and_then(|value| value.to_str().ok())
    })
}
