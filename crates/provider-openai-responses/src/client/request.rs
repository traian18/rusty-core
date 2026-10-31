//! Building the request: the body, the HTTP call, and sending it.

use rusty_model_backend::events::ModelError;
use rusty_model_backend::request::ModelRequest;
use serde_json::Value;

use super::OpenAiResponsesClient;
use crate::wire::{
    build_system_message, convert_messages_with_tool_ids, reasoning_effort_to_responses,
    tool_descriptor_to_responses, OpenAiResponsesRequest, ResponsesInputContentPart,
    ResponsesInputItem, RESPONSES_MIN_OUTPUT_TOKENS,
};

impl OpenAiResponsesClient {
    /// The JSON body for `request`, and the system message when the endpoint takes it as
    /// `instructions` instead of an input item.
    pub(super) fn build_body(
        &self,
        request: &ModelRequest,
    ) -> Result<(Value, Option<ResponsesInputItem>), ModelError> {
        let system = build_system_message(&request.system_prompt, &request.messages);
        let mut input = Vec::new();
        if let Some(system) = system.clone().filter(|_| !self.chatgpt_subscription) {
            input.push(system);
        }
        {
            let tool_ids = self
                .tool_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            input.extend(convert_messages_with_tool_ids(&request.messages, &tool_ids));
        }

        let max_output_tokens = request
            .max_tokens
            .unwrap_or(self.config.default_max_tokens)
            .max(RESPONSES_MIN_OUTPUT_TOKENS);
        let responses_request = OpenAiResponsesRequest {
            model: request
                .model
                .clone()
                .unwrap_or_else(|| self.config.default_model.clone()),
            input,
            // An explicit empty array prevents provider_options from introducing
            // provider-native tools when the harness grants no tools.
            tools: Some(
                request
                    .tools
                    .iter()
                    .map(tool_descriptor_to_responses)
                    .collect(),
            ),
            max_output_tokens: Some(max_output_tokens),
            temperature: request.temperature,
            reasoning: request.reasoning_effort.map(reasoning_effort_to_responses),
            stream: true,
            store: false,
        };
        let mut body = rusty_model_backend::merge_provider_options(
            serde_json::to_value(&responses_request).map_err(|error| {
                ModelError::InvalidRequest {
                    message: format!("failed to serialize request: {error}"),
                }
            })?,
            &request.provider_options,
            "openai-responses",
        );
        if let (Some(key), Some(object)) = (&request.cache_key, body.as_object_mut()) {
            object
                .entry("prompt_cache_key")
                .or_insert_with(|| key.clone().into());
        }
        Ok((body, system))
    }

    /// The POST to `{base_url}/responses` with the configured key and headers.
    pub(super) fn post_builder(&self) -> reqwest::RequestBuilder {
        let url = format!("{}/responses", self.config.base_url);
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

/// Reshapes the body for the ChatGPT subscription endpoint: the system prompt becomes
/// `instructions`, and the knobs that endpoint owns (output budget, temperature) are removed.
pub(super) fn prepare_chatgpt_body(
    body: &mut Value,
    system: Option<ResponsesInputItem>,
) -> Result<(), ModelError> {
    let instructions = match system {
        Some(ResponsesInputItem::InputMessage { content, .. }) => content
            .into_iter()
            .filter_map(|part| match part {
                ResponsesInputContentPart::InputText { text } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let object = body
        .as_object_mut()
        .ok_or_else(|| ModelError::InvalidRequest {
            message: "the Responses request body is not a JSON object".to_string(),
        })?;
    object.insert("instructions".into(), instructions.into());
    object.remove("max_output_tokens");
    object.remove("temperature");
    object.insert("store".into(), false.into());
    object.insert("stream".into(), true.into());
    Ok(())
}
