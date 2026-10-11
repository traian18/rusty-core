//! Provider-neutral `ModelClient` adapter for the runtime `ExecutionBackend`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use harness_model::client::ModelClient;
use harness_model::events::{ModelError, ModelEvent, ModelResult};
use harness_model::request::ModelRequest;
use harness_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionResult,
};
use harness_protocol::ids::{BackendId, RequestId};
use harness_protocol::tools::ToolCall;
use harness_runtime::traits::ExecutionBackend;

/// Adapts any provider-neutral model client to the harness backend contract.
pub struct GenericModelBackend {
    model_client: Arc<dyn ModelClient>,
    descriptor: BackendDescriptor,
    capabilities: BackendCapabilities,
    recovery: RecoveryPolicy,
    circuit: Mutex<CircuitState>,
}

/// Bounded recovery settings for one provider backend instance.
///
/// Serializable so it can be embedded directly in a provider config struct
/// (e.g. `AnthropicConfig::recovery`) and set from the RPC-supplied
/// `integration_config` JSON, rather than only being constructible in code.
/// Fields round-trip through JSON as whole seconds, matching the convention
/// every provider config in this workspace already uses for its own
/// `request_timeout_secs` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecoveryPolicy {
    /// Total calls allowed for a request, including its initial attempt.
    pub max_attempts: usize,
    /// Maximum wait for the first stream event of an attempt. Longer than
    /// `idle_timeout` because the provider may queue the request or think
    /// silently before emitting anything.
    #[serde(
        rename = "first_event_timeout_secs",
        serialize_with = "serialize_duration_secs",
        deserialize_with = "deserialize_duration_secs"
    )]
    pub first_event_timeout: Duration,
    /// Maximum inactivity once the stream has started; reset on every model
    /// stream event. Active streams have no wall-clock deadline.
    #[serde(
        rename = "idle_timeout_secs",
        alias = "total_deadline_secs",
        serialize_with = "serialize_duration_secs",
        deserialize_with = "deserialize_duration_secs"
    )]
    pub idle_timeout: Duration,
    /// Consecutive transient request failures that open the circuit.
    pub circuit_failure_threshold: u32,
    /// How long an open circuit fails fast before one probe is allowed.
    #[serde(
        rename = "circuit_open_duration_secs",
        serialize_with = "serialize_duration_secs",
        deserialize_with = "deserialize_duration_secs"
    )]
    pub circuit_open_duration: Duration,
}

fn serialize_duration_secs<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_u64(duration.as_secs())
}

fn deserialize_duration_secs<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Duration::from_secs(u64::deserialize(deserializer)?))
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 2,
            first_event_timeout: Duration::from_secs(180),
            idle_timeout: Duration::from_secs(120),
            circuit_failure_threshold: 3,
            circuit_open_duration: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Default)]
struct CircuitState {
    consecutive_failures: u32,
    open_until: Option<tokio::time::Instant>,
    half_open_probe_in_flight: bool,
}

#[derive(Default)]
struct AttemptProgress {
    text: String,
    requested_tools: bool,
}

struct AttemptOutcome {
    result: Result<ModelResult, ModelError>,
    emitted_output: bool,
}

/// How far a model client may run ahead of `run_attempt` before its next
/// `send` waits. Sized for a large SSE chunk's worth of deltas; anything
/// beyond it is backpressure, never loss.
const MODEL_EVENT_BUFFER: usize = 256;

/// Why `run_attempt`'s receive loop stopped, decided before the client task
/// is joined so every exit path closes the channel first.
enum AttemptExit {
    Completed(ModelResult),
    Error(ModelError),
    /// The client dropped its sender without a terminal event.
    Closed,
    Cancelled,
    TimedOut,
}

impl GenericModelBackend {
    pub fn new(model_client: Arc<dyn ModelClient>) -> Self {
        Self::new_with_recovery(model_client, RecoveryPolicy::default())
    }

    /// Construct a backend with an explicit, per-provider recovery policy.
    pub fn new_with_recovery(model_client: Arc<dyn ModelClient>, recovery: RecoveryPolicy) -> Self {
        let model = model_client.capabilities();
        let capabilities = BackendCapabilities {
            streaming: model.streaming,
            reasoning_stream: model.reasoning,
            tool_calls: model.tool_calls,
            parallel_tool_calls: model.parallel_tool_calls,
            images: model.images,
            structured_output: model.structured_output,
            host_managed_tools: true,
            ..Default::default()
        };
        Self {
            model_client,
            descriptor: BackendDescriptor {
                id: BackendId::new(),
                name: "generic-model-backend".to_string(),
                description: "Provider-neutral model backend".to_string(),
                capabilities: capabilities.clone(),
            },
            capabilities,
            recovery,
            circuit: Mutex::new(CircuitState::default()),
        }
    }

    /// The recovery policy this backend was constructed with. Exposed mainly
    /// for tests that verify a provider config's `recovery` field was
    /// actually threaded through to the backend, rather than silently
    /// falling back to the default.
    pub fn recovery_policy(&self) -> &RecoveryPolicy {
        &self.recovery
    }

    /// Validates the request against this backend's advertised capabilities
    /// *before* any network call is made, so an unsupported request never
    /// causes a billed call to the provider. Returns `Some(error)` to reject
    /// the request, or `None` to proceed.
    fn check_capabilities(
        &self,
        request: &harness_protocol::backend::ExecutionRequest,
    ) -> Option<ModelError> {
        let wants_reasoning =
            request.extended_thinking || request.params.reasoning_effort.is_some();
        if wants_reasoning && !self.capabilities.reasoning_stream {
            return Some(ModelError::UnsupportedCapability {
                capability: "reasoning".to_string(),
                detail: format!(
                    "{} does not support reasoning/extended thinking",
                    self.descriptor.name
                ),
            });
        }

        let wants_images = request.messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    harness_protocol::messages::ContentBlock::Image { .. }
                )
            })
        });
        if wants_images && !self.capabilities.images {
            return Some(ModelError::UnsupportedCapability {
                capability: "images".to_string(),
                detail: format!("{} does not support image input", self.descriptor.name),
            });
        }

        if !request.tools.is_empty() && !self.capabilities.tool_calls {
            return Some(ModelError::UnsupportedCapability {
                capability: "tool_calls".to_string(),
                detail: format!("{} does not support tool calls", self.descriptor.name),
            });
        }

        // A caller asking for JSON and silently receiving prose is the worst
        // outcome here — it fails later, at a parse site far from the cause.
        // `ResponseFormat::Text` is the provider default, so it needs no
        // capability.
        let wants_structured_output = !matches!(
            request.params.response_format,
            None | Some(harness_protocol::backend::ResponseFormat::Text)
        );
        if wants_structured_output && !self.capabilities.structured_output {
            return Some(ModelError::UnsupportedCapability {
                capability: "structured_output".to_string(),
                detail: format!(
                    "{} cannot constrain its response format",
                    self.descriptor.name
                ),
            });
        }

        None
    }

    fn circuit_allows_request(&self) -> Result<(), ModelError> {
        let now = tokio::time::Instant::now();
        let mut circuit = self.circuit.lock().expect("circuit mutex poisoned");
        if let Some(open_until) = circuit.open_until {
            if open_until > now {
                return Err(ModelError::CircuitOpen {
                    retry_after: open_until.duration_since(now),
                });
            }
            if circuit.half_open_probe_in_flight {
                return Err(ModelError::CircuitOpen {
                    retry_after: self.recovery.circuit_open_duration,
                });
            }
            circuit.half_open_probe_in_flight = true;
        }
        Ok(())
    }

    fn record_success(&self) {
        let mut circuit = self.circuit.lock().expect("circuit mutex poisoned");
        *circuit = CircuitState::default();
    }

    fn record_failure(&self, error: &ModelError) {
        let mut circuit = self.circuit.lock().expect("circuit mutex poisoned");
        circuit.half_open_probe_in_flight = false;
        if !error.is_retryable() {
            return;
        }
        circuit.consecutive_failures += 1;
        if circuit.consecutive_failures >= self.recovery.circuit_failure_threshold {
            circuit.open_until =
                Some(tokio::time::Instant::now() + self.recovery.circuit_open_duration);
        }
    }

    async fn run_attempt(
        &self,
        model_request: ModelRequest,
        request_id: RequestId,
        sink: &broadcast::Sender<ExecutionEvent>,
        cancel: &CancellationToken,
        progress: &mut AttemptProgress,
    ) -> AttemptOutcome {
        // Bounded and lossless: see `harness_model::client::ModelEventSender`
        // for why this is not a `broadcast` channel. The capacity only sets
        // how far the client may run ahead before `send` makes it wait.
        let (model_tx, mut model_rx) = mpsc::channel(MODEL_EVENT_BUFFER);
        let client = self.model_client.clone();
        let attempt_cancel = cancel.child_token();
        let task_cancel = attempt_cancel.clone();
        #[allow(unused_mut)]
        let mut stream =
            tokio::spawn(async move { client.stream(model_request, model_tx, task_cancel).await });
        let mut emitted_output = false;
        let mut requested_tools = false;
        // Waiting for the first event may legitimately take longer than a gap
        // mid-stream, so it gets its own limit.
        let mut deadline = tokio::time::Instant::now() + self.recovery.first_event_timeout;

        let exit = loop {
            tokio::select! {
                message = model_rx.recv() => match message {
                    Some(event) => {
                        deadline = tokio::time::Instant::now() + self.recovery.idle_timeout;
                        match event {
                            ModelEvent::Completed { mut result } => {
                                if requested_tools {
                                    result.stop_reason = "tool_use".into();
                                }
                                break AttemptExit::Completed(result);
                            }
                            ModelEvent::Error { error } => break AttemptExit::Error(error),
                            event => {
                                requested_tools |= matches!(event, ModelEvent::ToolCallCompleted { .. });
                                progress.requested_tools = requested_tools;
                                if let ModelEvent::TextDelta { delta } = &event {
                                    progress.text.push_str(delta);
                                }
                                emitted_output |= matches!(event, ModelEvent::TextDelta { .. } | ModelEvent::ReasoningDelta { .. } | ModelEvent::ToolCallCompleted { .. } | ModelEvent::UsageUpdate { .. });
                                let _ = Self::translate_event(event, request_id, sink);
                            }
                        }
                    }
                    None => break AttemptExit::Closed,
                },
                _ = cancel.cancelled() => break AttemptExit::Cancelled,
                _ = tokio::time::sleep_until(deadline) => break AttemptExit::TimedOut,
            }
        };

        // Nothing past this point reads the channel, so close it before
        // joining the client task: a client still holding events for a
        // consumer that has left would otherwise block on a full buffer and
        // this join would never return.
        drop(model_rx);
        let result = match exit {
            AttemptExit::Completed(result) => {
                let _ = stream.await;
                Ok(result)
            }
            AttemptExit::Error(error) => {
                let _ = stream.await;
                Err(error)
            }
            AttemptExit::Closed => {
                let mut result = match stream.await {
                    Ok(result) => result,
                    Err(error) => Err(ModelError::BackendError {
                        message: format!("model client task failed: {error}"),
                        code: "TASK_PANIC".to_string(),
                    }),
                };
                // Some clients return their result without emitting a
                // Completed event. Tools still require a follow-up turn
                // even when the provider labels the completion STOP.
                if requested_tools {
                    if let Ok(result) = &mut result {
                        result.stop_reason = "tool_use".into();
                    }
                }
                result
            }
            AttemptExit::Cancelled => {
                attempt_cancel.cancel();
                let _ = stream.await;
                Err(ModelError::Cancelled)
            }
            AttemptExit::TimedOut => {
                attempt_cancel.cancel();
                let _ = stream.await;
                Err(ModelError::Timeout)
            }
        };
        AttemptOutcome {
            result,
            emitted_output,
        }
    }

    fn retry_delay(&self, attempt: usize, error: &ModelError) -> Duration {
        if let Some(delay) = error.retry_after() {
            return delay;
        }
        let base_ms = 250_u64.saturating_mul(1_u64 << (attempt.saturating_sub(1) as u32));
        let jitter_ms = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_millis() as u64)
            % (base_ms / 2 + 1);
        Duration::from_millis(base_ms + jitter_ms)
    }

    fn translate_event(
        event: ModelEvent,
        request_id: RequestId,
        sink: &broadcast::Sender<ExecutionEvent>,
    ) -> Option<Result<ExecutionResult, ExecutionError>> {
        match event {
            ModelEvent::TextDelta { delta } => {
                let _ = sink.send(ExecutionEvent::TextDelta { request_id, delta });
                None
            }
            ModelEvent::ReasoningDelta { delta } => {
                let _ = sink.send(ExecutionEvent::ReasoningDelta { request_id, delta });
                None
            }
            ModelEvent::ToolCallStarted { .. } | ModelEvent::ToolCallDelta { .. } => None,
            ModelEvent::ToolCallCompleted { id, name, input } => {
                let _ = sink.send(ExecutionEvent::ToolCallRequested {
                    request_id,
                    call: ToolCall {
                        id,
                        name,
                        arguments: input,
                    },
                });
                None
            }
            ModelEvent::UsageUpdate { usage } => {
                let _ = sink.send(ExecutionEvent::UsageUpdate { request_id, usage });
                None
            }
            ModelEvent::Completed { result } => Some(Ok(to_execution_result(request_id, result))),
            ModelEvent::Error { error } => Some(Err(to_execution_error(error))),
        }
    }
}

fn to_execution_result(request_id: RequestId, result: ModelResult) -> ExecutionResult {
    ExecutionResult {
        request_id,
        usage: result.usage,
        cost: result.cost,
        // The agent loop uses `tool_use` to keep the run alive until tool
        // results arrive. Chat Completions uses `tool_calls` instead; passing
        // it through would finish the run while its tools are still running.
        finish_reason: match result.stop_reason.as_str() {
            "tool_calls" | "function_call" => "tool_use".to_string(),
            _ => result.stop_reason,
        },
    }
}

fn to_execution_error(error: ModelError) -> ExecutionError {
    match error {
        ModelError::BackendError { message, code } => {
            ExecutionError::BackendError { message, code }
        }
        ModelError::RateLimited { retry_after } => ExecutionError::RateLimited {
            retry_after: retry_after.map(|delay| {
                let millis = delay.as_millis().min(u128::from(u64::MAX)) as u64;
                millis.saturating_add(999) / 1_000
            }),
        },
        ModelError::InvalidRequest { message } => ExecutionError::InvalidRequest { message },
        ModelError::Cancelled => ExecutionError::Cancelled,
        ModelError::Timeout => ExecutionError::Timeout,
        ModelError::CircuitOpen { retry_after } => ExecutionError::BackendError {
            message: format!(
                "provider circuit is open; retry after {} ms",
                retry_after.as_millis()
            ),
            code: "CIRCUIT_OPEN".to_string(),
        },
        ModelError::Protocol { message } => ExecutionError::BackendError {
            message,
            code: "PROTOCOL_ERROR".to_string(),
        },
        ModelError::StreamInterrupted { message } => ExecutionError::BackendError {
            message,
            code: "STREAM_INTERRUPTED".to_string(),
        },
        ModelError::UnsupportedCapability { capability, detail } => {
            ExecutionError::UnsupportedCapability { capability, detail }
        }
    }
}

fn emit_terminal(
    sink: &broadcast::Sender<ExecutionEvent>,
    request_id: RequestId,
    result: &Result<ExecutionResult, ExecutionError>,
) {
    match result {
        Ok(result) => {
            let _ = sink.send(ExecutionEvent::Completed {
                request_id,
                result: result.clone(),
            });
        }
        Err(error) => {
            let _ = sink.send(ExecutionEvent::Error {
                request_id,
                error: error.clone(),
            });
        }
    }
}

#[async_trait]
impl ExecutionBackend for GenericModelBackend {
    fn descriptor(&self) -> BackendDescriptor {
        self.descriptor.clone()
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.capabilities.clone()
    }

    async fn execute(
        &self,
        request: harness_protocol::backend::ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        let request_id = request.request_id;
        let call_start = tokio::time::Instant::now();
        let backend_label = self.descriptor.name.clone();
        if let Some(error) = self.check_capabilities(&request) {
            metrics::counter!("harness_backend_requests_total", "backend" => backend_label.clone(), "outcome" => "rejected_capability").increment(1);
            let final_result = Err(to_execution_error(error));
            emit_terminal(&sink, request_id, &final_result);
            return final_result;
        }
        let mut model_request = ModelRequest {
            system_prompt: request.system_prompt,
            messages: request.messages,
            tools: request.tools,
            model: request.params.model,
            max_tokens: request.params.max_tokens,
            temperature: request.params.temperature,
            stop_sequences: request.params.stop_sequences,
            extended_thinking: request.extended_thinking,
            reasoning_effort: request.params.reasoning_effort,
            response_format: request.params.response_format,
            provider_options: request.params.provider_options,
        };
        if let Err(error) = self.circuit_allows_request() {
            metrics::counter!("harness_backend_requests_total", "backend" => backend_label.clone(), "outcome" => "rejected_circuit_open").increment(1);
            let final_result = Err(to_execution_error(error));
            emit_terminal(&sink, request_id, &final_result);
            return final_result;
        }

        let mut attempt = 1;
        let final_result = loop {
            let mut progress = AttemptProgress::default();
            let outcome = self
                .run_attempt(
                    model_request.clone(),
                    request_id,
                    &sink,
                    &cancel,
                    &mut progress,
                )
                .await;

            match outcome.result {
                Ok(result) => {
                    self.record_success();
                    break Ok(to_execution_result(request_id, result));
                }
                Err(error) if cancel.is_cancelled() || matches!(error, ModelError::Cancelled) => {
                    break Err(ExecutionError::Cancelled);
                }
                Err(error)
                    if (!outcome.emitted_output
                        || (!progress.text.is_empty()
                            && !progress.requested_tools
                            && matches!(
                                model_request.response_format,
                                None | Some(harness_protocol::backend::ResponseFormat::Text)
                            )))
                        && error.is_retryable()
                        && attempt < self.recovery.max_attempts =>
                {
                    let delay = self.retry_delay(attempt, &error);
                    if delay >= self.recovery.idle_timeout {
                        self.record_failure(&error);
                        break Err(to_execution_error(error));
                    }
                    if !progress.text.is_empty() {
                        use harness_protocol::ids::{MessageId, Timestamp};
                        use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
                        // Continue the answer in context, never replay the original
                        // request after visible output or repeat side effects.
                        for (role, text) in [
                            (MessageRole::Assistant, progress.text),
                            (MessageRole::User, "The response stream was interrupted. Continue exactly where your previous answer stopped. Output only the remaining text, without repeating earlier content or adding an introduction. Use the research and tool results already in this conversation; do not perform further tool calls.".into()),
                        ] {
                            model_request.messages.push(AgentMessage {
                                id: MessageId::new(), role,
                                content: vec![ContentBlock::Text { text }],
                                created_at: Timestamp::now(),
                            });
                        }
                        model_request.tools.clear();
                    }
                    warn!(attempt, ?delay, error = %error, "retrying transient model provider failure");
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => { attempt += 1; }
                        _ = cancel.cancelled() => break Err(ExecutionError::Cancelled),
                    }
                }
                Err(error) => {
                    self.record_failure(&error);
                    break Err(to_execution_error(error));
                }
            }
        };
        if final_result.is_err() {
            if let Err(ExecutionError::Timeout) = &final_result {
                self.record_failure(&ModelError::Timeout);
            }
        }
        metrics::histogram!("harness_backend_request_duration_seconds", "backend" => backend_label.clone())
            .record(call_start.elapsed().as_secs_f64());
        metrics::counter!(
            "harness_backend_requests_total",
            "backend" => backend_label.clone(),
            "outcome" => if final_result.is_ok() { "success" } else { "error" }
        )
        .increment(1);
        metrics::counter!("harness_backend_request_attempts_total", "backend" => backend_label)
            .increment(attempt as u64);
        metrics::gauge!("harness_backend_circuit_open", "backend" => self.descriptor.name.clone())
            .set(
                if self
                    .circuit
                    .lock()
                    .expect("circuit mutex poisoned")
                    .open_until
                    .is_some()
                {
                    1.0
                } else {
                    0.0
                },
            );
        emit_terminal(&sink, request_id, &final_result);
        final_result
    }
}

#[cfg(test)]
mod tests;
