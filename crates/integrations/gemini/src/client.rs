//! Gemini `streamGenerateContent` API client implementing [`ModelClient`].
//!
//! [`ModelClient`]: harness_model::client::ModelClient

use async_trait::async_trait;
use tracing::instrument;

use harness_model::client::ModelClient;
use harness_model::events::{ModelError, ModelEvent, ModelResult};
use harness_model::request::{ModelCapabilities, ModelRequest};

use crate::config::GeminiConfig;
use crate::wire::{
    build_system_instruction, convert_messages, tool_descriptor_to_gemini, GeminiGenerationConfig,
    GeminiRequest, GeminiSseParser, GeminiTool,
};

/// Client for the Gemini `streamGenerateContent` API.
///
/// Implements [`ModelClient`] by converting [`ModelRequest`] into the Gemini
/// wire format, sending an HTTP POST to
/// `{base_url}/models/{model}:streamGenerateContent?alt=sse&key={api_key}`
/// (Gemini authenticates via an API key **query parameter**, not a header —
/// unlike Anthropic/OpenAI), and parsing the SSE response stream.
pub struct GeminiClient {
    config: GeminiConfig,
    http_client: reqwest::Client,
}

impl GeminiClient {
    pub fn new(config: GeminiConfig) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .build()
            .expect("reqwest::ClientBuilder::build should not fail with default settings");
        Self {
            config,
            http_client,
        }
    }
}

#[async_trait]
impl ModelClient for GeminiClient {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            streaming: true,
            reasoning: false,
            tool_calls: true,
            parallel_tool_calls: true,
            images: true,
        }
    }

    #[instrument(skip(self, request, events, cancel))]
    async fn stream(
        &self,
        request: ModelRequest,
        events: tokio::sync::broadcast::Sender<ModelEvent>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ModelResult, ModelError> {
        let model = request
            .model
            .clone()
            .unwrap_or_else(|| self.config.default_model.clone());

        let gemini_request = GeminiRequest {
            contents: convert_messages(&request.messages),
            system_instruction: build_system_instruction(&request.system_prompt, &request.messages),
            tools: if request.tools.is_empty() {
                None
            } else {
                Some(vec![GeminiTool {
                    function_declarations: request
                        .tools
                        .iter()
                        .map(tool_descriptor_to_gemini)
                        .collect(),
                }])
            },
            generation_config: GeminiGenerationConfig {
                max_output_tokens: Some(
                    request.max_tokens.unwrap_or(self.config.default_max_tokens),
                ),
                temperature: request.temperature,
                stop_sequences: (!request.stop_sequences.is_empty())
                    .then_some(request.stop_sequences),
            },
        };

        let url = format!(
            "{}/models/{}:streamGenerateContent",
            self.config.base_url, model
        );

        // M4: merge caller-supplied `provider_options["gemini"]` knobs
        // (e.g. `topK`, `topP`) that have no typed field on
        // `GeminiGenerationConfig` — see `harness_model::merge_provider_options`'s
        // doc comment for the precedence rule (typed fields above always
        // win). Gemini's generation-tuning knobs all live nested under
        // `generationConfig` on the wire, not at the request's top level,
        // so the merge targets that nested object specifically.
        let mut body =
            serde_json::to_value(&gemini_request).map_err(|error| ModelError::InvalidRequest {
                message: format!("failed to serialize request: {error}"),
            })?;
        if let Some(generation_config) = body.get_mut("generationConfig") {
            *generation_config = harness_model::merge_provider_options(
                generation_config.take(),
                &request.provider_options,
                "gemini",
            );
        }

        let response = self
            .http_client
            .post(&url)
            .query(&[("alt", "sse"), ("key", self.config.api_key.as_str())])
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
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
            Self::handle_success_response(response, &events, &cancel, model).await
        } else if status.as_u16() == 429 {
            Self::handle_rate_limit(response)
        } else {
            Self::handle_error_response(response, status).await
        }
    }
}

impl GeminiClient {
    async fn handle_success_response(
        mut response: reqwest::Response,
        events: &tokio::sync::broadcast::Sender<ModelEvent>,
        cancel: &tokio_util::sync::CancellationToken,
        model: String,
    ) -> Result<ModelResult, ModelError> {
        let mut parser = GeminiSseParser::new(model);

        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return Err(ModelError::Cancelled),
                chunk = response.chunk() => chunk.map_err(|error| {
                    if error.is_timeout() {
                        ModelError::Timeout
                    } else {
                        ModelError::Protocol {
                            message: format!("failed to read Gemini SSE stream: {error}"),
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

/// Normalize Gemini's standard `Retry-After` header (whole seconds) using the
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
        headers.insert(reqwest::header::RETRY_AFTER, "2".parse().unwrap());
        assert_eq!(
            retry_after_from_headers(&headers),
            Some(Duration::from_secs(2))
        );
    }

    #[test]
    fn missing_header_normalizes_to_none() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after_from_headers(&headers), None);
    }
}
