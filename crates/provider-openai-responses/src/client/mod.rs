//! OpenAI Responses API client implementing [`ModelClient`].
//!
//! `request` builds and sends the call; `response` reads what comes back.
//!
//! [`ModelClient`]: rusty_model_backend::client::ModelClient

mod request;
mod response;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tracing::instrument;

use rusty_model_backend::client::{ModelClient, ModelEventSender};
use rusty_model_backend::events::{ModelError, ModelResult};
use rusty_model_backend::request::{ModelCapabilities, ModelRequest};

use crate::config::OpenAiResponsesConfig;
use crate::wire::ProviderToolIds;
use request::prepare_chatgpt_body;

pub use rusty_model_backend::auth::InferenceAuth as ResponsesAuth;

/// Client for the OpenAI Responses API.
///
/// Implements [`ModelClient`] by converting [`ModelRequest`] into the
/// Responses wire format, sending HTTP POST requests to
/// `{base_url}/responses`, and parsing the SSE response stream into
/// [`ModelEvent`](rusty_model_backend::events::ModelEvent)s.
pub struct OpenAiResponsesClient {
    config: OpenAiResponsesConfig,
    http_client: reqwest::Client,
    tool_ids: ProviderToolIds,
    auth: Option<Arc<dyn ResponsesAuth>>,
    chatgpt_subscription: bool,
}

impl OpenAiResponsesClient {
    pub fn new(config: OpenAiResponsesConfig) -> Self {
        let http_client = reqwest::Client::builder()
            .read_timeout(config.request_timeout)
            .build()
            .expect("reqwest::ClientBuilder::build should not fail with default settings");
        Self {
            config,
            http_client,
            tool_ids: Arc::new(Mutex::new(HashMap::new())),
            auth: None,
            chatgpt_subscription: false,
        }
    }

    pub fn with_auth(mut self, auth: Arc<dyn ResponsesAuth>) -> Self {
        self.auth = Some(auth);
        self
    }

    pub fn with_chatgpt_auth(mut self, auth: Arc<dyn ResponsesAuth>) -> Self {
        self.auth = Some(auth);
        self.chatgpt_subscription = true;
        self
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
        events: ModelEventSender,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ModelResult, ModelError> {
        let (mut body, system) = self.build_body(&request)?;
        let mut builder = self.post_builder();
        if self.chatgpt_subscription {
            prepare_chatgpt_body(&mut body, system)?;
            // The subscription backend routes its prompt cache by the conversation, as the Codex
            // CLI sends it, not by the body alone.
            if let Some(key) = &request.cache_key {
                builder = builder
                    .header("session_id", key)
                    .header("conversation_id", key);
            }
        }
        let response = self.send_request(builder, &body, &cancel).await?;
        self.read_response(response, &events, &cancel).await
    }
}

#[cfg(test)]
mod tests;
