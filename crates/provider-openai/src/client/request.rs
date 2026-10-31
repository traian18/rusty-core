//! Building the request: the body, the HTTP call, and sending it.

use rusty_model_backend::events::ModelError;
use rusty_model_backend::request::ModelRequest;
use serde_json::Value;

use super::OpenAiClient;
use crate::wire::{
    build_system_message, convert_messages_with_tool_ids, tool_descriptor_to_openai, OpenAiRequest,
    StreamOptions,
};

impl OpenAiClient {
    /// The JSON body for `request`, with provider options and cache hints merged in.
    pub(super) fn build_body(&self, request: &ModelRequest) -> Result<Value, ModelError> {
        let mut messages = Vec::new();
        if let Some(system) = build_system_message(&request.system_prompt, &request.messages) {
            messages.push(system);
        }
        {
            let tool_ids = self
                .tool_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            messages.extend(convert_messages_with_tool_ids(&request.messages, &tool_ids));
        }
        let openai_request = OpenAiRequest {
            model: request
                .model
                .clone()
                .unwrap_or_else(|| self.config.default_model.clone()),
            messages,
            tools: (!request.tools.is_empty()).then(|| {
                request
                    .tools
                    .iter()
                    .map(tool_descriptor_to_openai)
                    .collect()
            }),
            max_tokens: Some(request.max_tokens.unwrap_or(self.config.default_max_tokens)),
            temperature: request.temperature,
            stop: (!request.stop_sequences.is_empty()).then(|| request.stop_sequences.clone()),
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
        // Caller-supplied `provider_options["openai"]` knobs (such as `top_p`) that have no typed
        // field are merged in; the typed fields above always win.
        let mut body = rusty_model_backend::merge_provider_options(
            serde_json::to_value(&openai_request).map_err(|error| ModelError::InvalidRequest {
                message: format!("failed to serialize request: {error}"),
            })?,
            &request.provider_options,
            "openai",
        );
        crate::cache::add_cache_hints(
            &mut body,
            self.config.prompt_cache,
            &self.config.base_url,
            request.cache_key.as_deref(),
        );
        Ok(body)
    }

    /// The POST to `{base_url}/chat/completions` with the configured key and headers.
    pub(super) fn post_builder(&self) -> reqwest::RequestBuilder {
        let url = format!("{}/chat/completions", self.config.base_url);
        let mut builder = self
            .http_client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .header("content-type", "application/json");
        for (key, value) in &self.config.extra_headers {
            builder = builder.header(key, value);
        }
        builder
    }

    /// Adds provider authentication (if any) and sends, giving up if `cancel` fires.
    pub(super) async fn send_request(
        &self,
        mut builder: reqwest::RequestBuilder,
        body: &Value,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<reqwest::Response, ModelError> {
        if let Some(auth) = &self.auth {
            let headers = tokio::select! {
                _ = cancel.cancelled() => return Err(ModelError::Cancelled),
                result = auth.headers(body) => result?,
            };
            builder = builder.headers(headers);
        }
        let sent = tokio::select! {
            _ = cancel.cancelled() => return Err(ModelError::Cancelled),
            result = builder.json(body).send() => result,
        };
        sent.map_err(|error| {
            if error.is_timeout() {
                ModelError::Timeout
            } else {
                ModelError::BackendError {
                    message: format!("HTTP request failed: {error}"),
                    code: "request_failed".to_string(),
                }
            }
        })
    }
}
