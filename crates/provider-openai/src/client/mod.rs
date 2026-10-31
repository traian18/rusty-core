//! OpenAI Chat Completions API client implementing [`ModelClient`].
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

use crate::config::OpenAiConfig;
use crate::wire::ProviderToolIds;

/// Client for the OpenAI Chat Completions API.
///
/// Implements [`ModelClient`] by converting [`ModelRequest`] into the OpenAI
/// wire format, sending HTTP POST requests to `{base_url}/chat/completions`,
/// and parsing the SSE response stream into [`ModelEvent`](rusty_model_backend::events::ModelEvent)s.
pub struct OpenAiClient {
    config: OpenAiConfig,
    http_client: reqwest::Client,
    tool_ids: ProviderToolIds,
    auth: Option<Arc<dyn rusty_model_backend::auth::InferenceAuth>>,
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
    pub fn with_auth(mut self, auth: Arc<dyn rusty_model_backend::auth::InferenceAuth>) -> Self {
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
        let body = self.build_body(&request)?;
        let response = self
            .send_request(self.post_builder(), &body, &cancel)
            .await?;
        self.read_response(response, &events, &cancel).await
    }
}

#[cfg(test)]
mod tests;
