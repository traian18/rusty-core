//! Building the request: the body, the model, and the HTTP call.

use rusty_model_backend::events::ModelError;
use rusty_model_backend::request::ModelRequest;

use crate::wire::{
    build_system, convert_messages_with_tool_ids, requires_adaptive_thinking,
    resolve_adaptive_thinking, resolve_thinking, tool_descriptor_to_anthropic, AnthropicRequest,
};

use super::AnthropicClient;

impl AnthropicClient {
    /// Builds the full merged JSON request body for a specific `max_tokens`.
    /// Extracted out of `stream()` so a request rejected for exceeding the
    /// model's real ceiling (see `stream()`'s 400 handling) can be rebuilt
    /// with the corrected value and resent -- including re-deriving the
    /// extended-thinking budget, which is itself a function of `max_tokens`
    /// and would otherwise still reference the rejected, too-high one.
    pub(super) fn build_body(
        &self,
        request: &ModelRequest,
        max_tokens: u64,
        adaptive_thinking: bool,
    ) -> Result<serde_json::Value, ModelError> {
        // Anthropic has no `response_format`; a non-text format is emulated
        // with a forced single-purpose tool call. See `structured_output_tool`.
        let structured_output = request
            .response_format
            .as_ref()
            .and_then(crate::wire::structured_output_tool);

        let (thinking, output_config) = if adaptive_thinking {
            resolve_adaptive_thinking(request.extended_thinking, request.reasoning_effort)
                .map_or((None, None), |(thinking, effort)| (Some(thinking), effort))
        } else {
            (
                resolve_thinking(
                    request.extended_thinking,
                    request.reasoning_effort,
                    max_tokens,
                )?,
                None,
            )
        };

        let anthropic_request = AnthropicRequest {
            model: self.model_for(request),
            system: if !request.system_prompt.is_empty() {
                Some(request.system_prompt.clone())
            } else {
                build_system(&request.messages)
            },
            messages: {
                let tool_ids = self
                    .tool_ids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            thinking,
            output_config,
            stream: true,
        };

        // M4: merge caller-supplied `provider_options["anthropic"]` knobs
        // (e.g. `top_k`) that have no typed field on `AnthropicRequest` —
        // see `rusty_model_backend::merge_provider_options`'s doc comment for the
        // precedence rule (typed fields above always win).
        let mut body = serde_json::to_value(&anthropic_request).map_err(|error| {
            ModelError::InvalidRequest {
                message: format!("failed to serialize request: {error}"),
            }
        })?;
        crate::cache::add_cache_breakpoints(&mut body);
        Ok(rusty_model_backend::merge_provider_options(
            body,
            &request.provider_options,
            "anthropic",
        ))
    }

    pub(super) fn model_for(&self, request: &ModelRequest) -> String {
        request
            .model
            .clone()
            .unwrap_or_else(|| self.config.default_model.clone())
    }

    /// Whether requests for `model` should use adaptive rather than budget
    /// thinking: known up front, or learned from an earlier rejection.
    pub(super) fn uses_adaptive_thinking(&self, model: &str) -> bool {
        requires_adaptive_thinking(model)
            || self
                .adaptive_thinking_models
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(model)
    }

    /// Builds the POST request (URL + auth/content headers), ready for
    /// `.json(body).send()`. Shared by the initial attempt and the
    /// corrected-`max_tokens` retry in `stream()`.
    pub(super) fn post_messages(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> reqwest::RequestBuilder {
        let mut request_builder = self
            .http_client
            .post(url)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json");

        if self.auth.is_none() && !self.config.api_key.is_empty() {
            request_builder = request_builder.header("x-api-key", &self.config.api_key);
            request_builder =
                request_builder.header("authorization", format!("Bearer {}", self.config.api_key));
        }
        for (key, value) in &self.config.extra_headers {
            request_builder = request_builder.header(key, value);
        }

        request_builder.json(body)
    }

    pub(super) async fn send_messages(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response, ModelError> {
        let mut builder = self.post_messages(url, body);
        if let Some(auth) = &self.auth {
            builder = builder.headers(auth.headers(body).await?);
        }
        builder.send().await.map_err(Self::map_send_error)
    }

    pub(super) fn map_send_error(error: reqwest::Error) -> ModelError {
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
