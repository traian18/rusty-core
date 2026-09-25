//! OpenAI Chat Completions API client implementing [`ModelClient`].
//!
//! [`ModelClient`]: harness_model::client::ModelClient

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tracing::instrument;

use harness_model::client::{send_event, ModelClient, ModelEventSender};
use harness_model::events::{ModelError, ModelResult};
use harness_model::request::{ModelCapabilities, ModelRequest};

use crate::config::OpenAiConfig;
use crate::wire::{
    build_system_message, convert_messages_with_tool_ids, tool_descriptor_to_openai, OpenAiRequest,
    OpenAiSseParser, ProviderToolIds, StreamOptions,
};

/// Client for the OpenAI Chat Completions API.
///
/// Implements [`ModelClient`] by converting [`ModelRequest`] into the OpenAI
/// wire format, sending HTTP POST requests to `{base_url}/chat/completions`,
/// and parsing the SSE response stream into [`ModelEvent`](harness_model::events::ModelEvent)s.
pub struct OpenAiClient {
    config: OpenAiConfig,
    http_client: reqwest::Client,
    tool_ids: ProviderToolIds,
    auth: Option<Arc<dyn harness_model::auth::InferenceAuth>>,
}

impl OpenAiClient {
    pub fn new(config: OpenAiConfig) -> Self {
        let http_client = reqwest::Client::builder()
            .read_timeout(config.request_timeout)
            .build()
            .expect("reqwest::ClientBuilder::build should not fail with default settings");
        Self {
            config,
            http_client,
            tool_ids: Arc::new(Mutex::new(HashMap::new())),
            auth: None,
        }
    }
    pub fn with_auth(mut self, auth: Arc<dyn harness_model::auth::InferenceAuth>) -> Self {
        self.auth = Some(auth);
        self
    }
}

#[async_trait]
impl ModelClient for OpenAiClient {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            streaming: true,
            // Opt-in per configured endpoint -- see `OpenAiConfig::
            // supports_reasoning`'s own doc comment for why this can't be a
            // blanket `true`: this same client also backs plain OpenAI and
            // arbitrary local/self-hosted endpoints, most of which reject an
            // unrecognized `reasoning_effort` param outright.
            reasoning: self.config.supports_reasoning,
            tool_calls: true,
            parallel_tool_calls: true,
            images: true,
            structured_output: true,
        }
    }

    #[instrument(skip(self, request, events, cancel))]
    async fn stream(
        &self,
        request: ModelRequest,
        events: ModelEventSender,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ModelResult, ModelError> {
        let mut messages = Vec::new();
        if let Some(system) = build_system_message(&request.system_prompt, &request.messages) {
            messages.push(system);
        }
        {
            let tool_ids = self.tool_ids.lock().expect("provider tool-id map poisoned");
            messages.extend(convert_messages_with_tool_ids(&request.messages, &tool_ids));
        }

        let openai_request = OpenAiRequest {
            model: request
                .model
                .unwrap_or_else(|| self.config.default_model.clone()),
            messages,
            tools: if request.tools.is_empty() {
                None
            } else {
                Some(
                    request
                        .tools
                        .iter()
                        .map(tool_descriptor_to_openai)
                        .collect(),
                )
            },
            max_tokens: Some(request.max_tokens.unwrap_or(self.config.default_max_tokens)),
            temperature: request.temperature,
            stop: (!request.stop_sequences.is_empty()).then_some(request.stop_sequences),
            response_format: request
                .response_format
                .as_ref()
                .and_then(crate::wire::OpenAiResponseFormat::from_neutral),
            reasoning_effort: request
                .reasoning_effort
                .filter(|_| self.config.supports_reasoning)
                .map(crate::wire::reasoning_effort_to_openai),
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
        };

        let url = format!("{}/chat/completions", self.config.base_url);

        let mut request_builder = self
            .http_client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .header("content-type", "application/json");
        for (key, value) in &self.config.extra_headers {
            request_builder = request_builder.header(key, value);
        }

        // M4: merge caller-supplied `provider_options["openai"]` knobs
        // (e.g. `top_p`, `frequency_penalty`) that have no typed field on
        // `OpenAiRequest` — see `harness_model::merge_provider_options`'s
        // doc comment for the precedence rule (typed fields above always
        // win).
        let body = harness_model::merge_provider_options(
            serde_json::to_value(&openai_request).map_err(|error| ModelError::InvalidRequest {
                message: format!("failed to serialize request: {error}"),
            })?,
            &request.provider_options,
            "openai",
        );

        if let Some(auth) = &self.auth {
            let headers = tokio::select! {
                _ = cancel.cancelled() => return Err(ModelError::Cancelled),
                result = auth.headers(&body) => result?,
            };
            request_builder = request_builder.headers(headers);
        }
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(ModelError::Cancelled),
            result = request_builder.json(&body).send() => result,
        }
        .map_err(|e| {
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

impl OpenAiClient {
    async fn handle_success_response(
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
        let (terminal_events, result) = parser.finish()?;
        for event in terminal_events {
            send_event(events, event, cancel).await?;
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

/// Normalize OpenAI's standard `Retry-After` header (whole seconds) using the
/// shared, provider-neutral parser.
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
