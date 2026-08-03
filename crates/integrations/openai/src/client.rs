//! OpenAI Chat Completions API client implementing [`ModelClient`].
//!
//! [`ModelClient`]: harness_model::client::ModelClient

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tracing::instrument;

use harness_model::client::ModelClient;
use harness_model::events::{ModelError, ModelEvent, ModelResult};
use harness_model::request::{ModelCapabilities, ModelRequest};

use crate::config::OpenAiConfig;
use crate::wire::{
    build_system_message, convert_messages_with_tool_ids, tool_descriptor_to_openai,
    OpenAiRequest, OpenAiSseParser, ProviderToolIds, StreamOptions,
};

/// Client for the OpenAI Chat Completions API.
///
/// Implements [`ModelClient`] by converting [`ModelRequest`] into the OpenAI
/// wire format, sending HTTP POST requests to `{base_url}/chat/completions`,
/// and parsing the SSE response stream into [`ModelEvent`]s.
pub struct OpenAiClient {
    config: OpenAiConfig,
    http_client: reqwest::Client,
    tool_ids: ProviderToolIds,
}

impl OpenAiClient {
    pub fn new(config: OpenAiConfig) -> Self {
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
impl ModelClient for OpenAiClient {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            streaming: true,
            reasoning: false,
            tool_calls: true,
            parallel_tool_calls: true,
        }
    }

    #[instrument(skip(self, request, events, cancel))]
    async fn stream(
        &self,
        request: ModelRequest,
        events: tokio::sync::broadcast::Sender<ModelEvent>,
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
            model: request.model.unwrap_or_else(|| self.config.default_model.clone()),
            messages,
            tools: if request.tools.is_empty() {
                None
            } else {
                Some(request.tools.iter().map(tool_descriptor_to_openai).collect())
            },
            max_tokens: Some(request.max_tokens.unwrap_or(self.config.default_max_tokens)),
            temperature: request.temperature,
            stop: (!request.stop_sequences.is_empty()).then_some(request.stop_sequences),
            stream: true,
            stream_options: StreamOptions { include_usage: true },
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

        let response = request_builder
            .json(&openai_request)
            .send()
            .await
            .map_err(|e| ModelError::BackendError {
                message: format!("HTTP request failed: {e}"),
                code: "request_failed".to_string(),
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
        events: &tokio::sync::broadcast::Sender<ModelEvent>,
        cancel: &tokio_util::sync::CancellationToken,
        tool_ids: ProviderToolIds,
    ) -> Result<ModelResult, ModelError> {
        let mut parser = OpenAiSseParser::with_tool_ids(tool_ids);

        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return Err(ModelError::Cancelled),
                chunk = response.chunk() => chunk.map_err(|error| ModelError::Protocol {
                    message: format!("failed to read OpenAI SSE stream: {error}"),
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
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
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
