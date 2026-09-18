//! OpenAI Responses API client implementing [`ModelClient`].
//!
//! [`ModelClient`]: harness_model::client::ModelClient

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tracing::instrument;

use harness_model::client::ModelClient;
use harness_model::events::{ModelError, ModelEvent, ModelResult};
use harness_model::request::{ModelCapabilities, ModelRequest};

use crate::config::OpenAiResponsesConfig;
use crate::wire::{
    build_system_message, convert_messages_with_tool_ids, reasoning_effort_to_responses,
    tool_descriptor_to_responses, OpenAiResponsesRequest, OpenAiResponsesSseParser,
    ProviderToolIds, RESPONSES_MIN_OUTPUT_TOKENS,
};

/// Client for the OpenAI Responses API.
///
/// Implements [`ModelClient`] by converting [`ModelRequest`] into the
/// Responses wire format, sending HTTP POST requests to
/// `{base_url}/responses`, and parsing the SSE response stream into
/// [`ModelEvent`]s.
pub struct OpenAiResponsesClient {
    config: OpenAiResponsesConfig,
    http_client: reqwest::Client,
    tool_ids: ProviderToolIds,
}

impl OpenAiResponsesClient {
    pub fn new(config: OpenAiResponsesConfig) -> Self {
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
impl ModelClient for OpenAiResponsesClient {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            streaming: true,
            // `reasoning_effort` maps onto the Responses API's own
            // `reasoning.effort` param (see `wire.rs`'s
            // `reasoning_effort_to_responses`); the response side already
            // parsed `response.reasoning_text.delta` events since this
            // crate's first version, so this was the one missing half.
            reasoning: true,
            tool_calls: true,
            parallel_tool_calls: true,
            images: true,
            // `response_format`/structured-output isn't mapped in this v1
            // either (Responses API's `text.format` param) -- same reasoning
            // as `reasoning` above.
            structured_output: false,
        }
    }

    #[instrument(skip(self, request, events, cancel))]
    async fn stream(
        &self,
        request: ModelRequest,
        events: tokio::sync::broadcast::Sender<ModelEvent>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ModelResult, ModelError> {
        let mut input = Vec::new();
        if let Some(system) = build_system_message(&request.system_prompt, &request.messages) {
            input.push(system);
        }
        {
            let tool_ids = self.tool_ids.lock().expect("provider tool-id map poisoned");
            input.extend(convert_messages_with_tool_ids(&request.messages, &tool_ids));
        }

        let max_output_tokens = request
            .max_tokens
            .unwrap_or(self.config.default_max_tokens)
            .max(RESPONSES_MIN_OUTPUT_TOKENS);

        let responses_request = OpenAiResponsesRequest {
            model: request
                .model
                .unwrap_or_else(|| self.config.default_model.clone()),
            input,
            tools: if request.tools.is_empty() {
                None
            } else {
                Some(
                    request
                        .tools
                        .iter()
                        .map(tool_descriptor_to_responses)
                        .collect(),
                )
            },
            max_output_tokens: Some(max_output_tokens),
            temperature: request.temperature,
            reasoning: request.reasoning_effort.map(reasoning_effort_to_responses),
            stream: true,
            store: false,
        };

        let url = format!("{}/responses", self.config.base_url);

        let mut request_builder = self
            .http_client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .header("content-type", "application/json");
        for (key, value) in &self.config.extra_headers {
            request_builder = request_builder.header(key, value);
        }

        let body = harness_model::merge_provider_options(
            serde_json::to_value(&responses_request).map_err(|error| ModelError::InvalidRequest {
                message: format!("failed to serialize request: {error}"),
            })?,
            &request.provider_options,
            "openai-responses",
        );

        let response = request_builder.json(&body).send().await.map_err(|e| {
            if e.is_timeout() {
                ModelError::Timeout
            } else {
                ModelError::BackendError {
                    message: format!("HTTP request failed: {e}"),
                    code: "request_failed".to_string(),
                }
            }
        })?;

        let status = response.status();
        if status.is_success() {
            Self::handle_success_response(response, &events, &cancel, self.tool_ids.clone()).await
        } else if status.as_u16() == 429 {
            Self::handle_rate_limit(response)
        } else {
            Self::handle_error_response(response, status).await
        }
    }
}

impl OpenAiResponsesClient {
    async fn handle_success_response(
        mut response: reqwest::Response,
        events: &tokio::sync::broadcast::Sender<ModelEvent>,
        cancel: &tokio_util::sync::CancellationToken,
        tool_ids: ProviderToolIds,
    ) -> Result<ModelResult, ModelError> {
        let mut parser = OpenAiResponsesSseParser::with_tool_ids(tool_ids);

        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return Err(ModelError::Cancelled),
                chunk = response.chunk() => chunk.map_err(|error| {
                    if error.is_timeout() {
                        ModelError::Timeout
                    } else {
                        ModelError::Protocol {
                            message: format!("failed to read Responses SSE stream: {error}"),
                        }
                    }
                })?,
            };
            let Some(chunk) = chunk else { break };

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

    fn handle_rate_limit(response: reqwest::Response) -> Result<ModelResult, ModelError> {
        let retry_after = retry_after_from_headers(response.headers());
        Err(ModelError::RateLimited { retry_after })
    }

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

fn retry_after_from_headers(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    harness_model::retry::parse_retry_after(|name| {
        headers.get(name).and_then(|value| value.to_str().ok())
    })
}

#[cfg(test)]
mod tests {
    use super::retry_after_from_headers;
    use std::time::Duration;

    #[test]
    fn standard_retry_after_is_normalized_to_duration() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "5".parse().unwrap());
        assert_eq!(
            retry_after_from_headers(&headers),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn missing_header_normalizes_to_none() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after_from_headers(&headers), None);
    }
}
