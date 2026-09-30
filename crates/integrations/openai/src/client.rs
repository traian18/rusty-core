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
use harness_protocol::usage::{ModelUsage, UsageValue};

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
            self.handle_success_response(response, &events, &cancel, self.tool_ids.clone())
                .await
        } else if status.as_u16() == 429 {
            Self::handle_rate_limit(response)
        } else {
            Self::handle_error_response(response, status).await
        }
    }
}

impl OpenAiClient {
    async fn handle_success_response(
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

fn is_openrouter_url(base_url: &str) -> bool {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| host == "openrouter.ai")
}

fn generation_usage(data: &serde_json::Value) -> Option<ModelUsage> {
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

async fn recover_openrouter_usage(
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
fn retry_after_from_headers(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    harness_model::retry::parse_retry_after(|name| {
        headers.get(name).and_then(|value| value.to_str().ok())
    })
}

#[cfg(test)]
mod tests {
    use super::{
        generation_usage, is_openrouter_url, recover_openrouter_usage, retry_after_from_headers,
    };
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

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

    #[test]
    fn openrouter_generation_usage_waits_for_real_counters() {
        assert!(is_openrouter_url("https://openrouter.ai/api/v1"));
        assert!(!is_openrouter_url("https://openrouter.ai.evil.test/api/v1"));
        assert!(generation_usage(&serde_json::json!({
            "native_tokens_prompt": 0,
            "native_tokens_completion": 0
        }))
        .is_none());
        let usage = generation_usage(&serde_json::json!({
            "native_tokens_prompt": 0,
            "tokens_prompt": 1_000,
            "native_tokens_completion": 0,
            "tokens_completion": 40,
            "native_tokens_cached": 100,
            "native_tokens_reasoning": 10
        }))
        .unwrap();
        assert_eq!(usage.input_tokens.value(), Some(1_000));
        assert_eq!(usage.output_tokens.value(), Some(40));
        assert_eq!(usage.cache_read_tokens.value(), Some(100));
        assert_eq!(usage.reasoning_tokens.value(), Some(10));
        assert_eq!(usage.total_tokens.value(), Some(1_040));
    }

    #[tokio::test]
    async fn generation_lookup_retries_until_openrouter_populates_usage() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for attempt in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0_u8; 4_096];
                let size = socket.read(&mut buffer).await.unwrap();
                let request = String::from_utf8_lossy(&buffer[..size]);
                assert!(request.starts_with("GET /api/v1/generation?id=gen-gemini HTTP/1.1"));
                let body = if attempt == 0 {
                    r#"{"data":{"native_tokens_prompt":0,"native_tokens_completion":0}}"#
                } else {
                    r#"{"data":{"native_tokens_prompt":200,"native_tokens_completion":30}}"#
                };
                let response = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let usage = recover_openrouter_usage(
            &reqwest::Client::new(),
            &format!("http://{address}/api/v1"),
            "sk-test",
            "gen-gemini",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(usage.total_tokens.value(), Some(230));
        server.await.unwrap();
    }
}
