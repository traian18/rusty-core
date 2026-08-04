//! Provider-neutral `ModelClient` adapter for the runtime `ExecutionBackend`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
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
#[derive(Debug, Clone)]
pub struct RecoveryPolicy {
    /// Total calls allowed for a request, including its initial attempt.
    pub max_attempts: usize,
    /// Deadline shared by all attempts and backoff delays.
    pub total_deadline: Duration,
    /// Consecutive transient request failures that open the circuit.
    pub circuit_failure_threshold: u32,
    /// How long an open circuit fails fast before one probe is allowed.
    pub circuit_open_duration: Duration,
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 2,
            total_deadline: Duration::from_secs(15),
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

struct AttemptOutcome {
    result: Result<ModelResult, ModelError>,
    emitted_output: bool,
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

    fn circuit_allows_request(&self) -> Result<(), ModelError> {
        let now = tokio::time::Instant::now();
        let mut circuit = self.circuit.lock().expect("circuit mutex poisoned");
        if let Some(open_until) = circuit.open_until {
            if open_until > now {
                return Err(ModelError::CircuitOpen { retry_after: open_until.duration_since(now) });
            }
            if circuit.half_open_probe_in_flight {
                return Err(ModelError::CircuitOpen { retry_after: self.recovery.circuit_open_duration });
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
            circuit.open_until = Some(tokio::time::Instant::now() + self.recovery.circuit_open_duration);
        }
    }

    async fn run_attempt(
        &self,
        model_request: ModelRequest,
        request_id: RequestId,
        sink: &broadcast::Sender<ExecutionEvent>,
        cancel: &CancellationToken,
        deadline: tokio::time::Instant,
    ) -> AttemptOutcome {
        let (model_tx, mut model_rx) = broadcast::channel(256);
        let client = self.model_client.clone();
        let attempt_cancel = cancel.child_token();
        let task_cancel = attempt_cancel.clone();
        let mut stream = tokio::spawn(async move { client.stream(model_request, model_tx, task_cancel).await });
        let mut emitted_output = false;

        loop {
            tokio::select! {
                message = model_rx.recv() => match message {
                    Ok(event) => {
                        match event {
                            ModelEvent::Completed { result } => {
                                let _ = stream.await;
                                return AttemptOutcome { result: Ok(result), emitted_output };
                            }
                            ModelEvent::Error { error } => {
                                let _ = stream.await;
                                return AttemptOutcome { result: Err(error), emitted_output };
                            }
                            event => {
                                emitted_output |= matches!(event, ModelEvent::TextDelta { .. } | ModelEvent::ReasoningDelta { .. } | ModelEvent::ToolCallCompleted { .. } | ModelEvent::UsageUpdate { .. });
                                let _ = Self::translate_event(event, request_id, sink);
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        attempt_cancel.cancel();
                        let _ = stream.await;
                        return AttemptOutcome { result: Err(ModelError::BackendError { message: format!("lost {count} model events"), code: "EVENT_LAG".to_string() }), emitted_output };
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        let result = match stream.await {
                            Ok(result) => result,
                            Err(error) => Err(ModelError::BackendError { message: format!("model client task failed: {error}"), code: "TASK_PANIC".to_string() }),
                        };
                        return AttemptOutcome { result, emitted_output };
                    }
                },
                _ = cancel.cancelled() => {
                    attempt_cancel.cancel();
                    let _ = stream.await;
                    return AttemptOutcome { result: Err(ModelError::Cancelled), emitted_output };
                }
                _ = tokio::time::sleep_until(deadline) => {
                    attempt_cancel.cancel();
                    let _ = stream.await;
                    return AttemptOutcome { result: Err(ModelError::Timeout), emitted_output };
                }
            }
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
            .subsec_millis() as u64) % (base_ms / 2 + 1);
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
            ModelEvent::Completed { result } => {
                Some(Ok(to_execution_result(request_id, result)))
            }
            ModelEvent::Error { error } => Some(Err(to_execution_error(error))),
        }
    }
}

fn to_execution_result(request_id: RequestId, result: ModelResult) -> ExecutionResult {
    ExecutionResult {
        request_id,
        usage: result.usage,
        cost: result.cost,
        finish_reason: result.stop_reason,
    }
}

fn to_execution_error(error: ModelError) -> ExecutionError {
    match error {
        ModelError::BackendError { message, code } => ExecutionError::BackendError { message, code },
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
            message: format!("provider circuit is open; retry after {} ms", retry_after.as_millis()),
            code: "CIRCUIT_OPEN".to_string(),
        },
        ModelError::Protocol { message } => ExecutionError::BackendError {
            message,
            code: "PROTOCOL_ERROR".to_string(),
        },
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
        let model_request = ModelRequest {
            system_prompt: request.system_prompt,
            messages: request.messages,
            tools: request.tools,
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: Vec::new(),
            extended_thinking: request.extended_thinking,
        };
        if let Err(error) = self.circuit_allows_request() {
            let final_result = Err(to_execution_error(error));
            emit_terminal(&sink, request_id, &final_result);
            return final_result;
        }

        let deadline = tokio::time::Instant::now() + self.recovery.total_deadline;
        let mut attempt = 1;
        let final_result = loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break Err(ExecutionError::Timeout);
            }
            let outcome = self
                .run_attempt(model_request.clone(), request_id, &sink, &cancel, deadline)
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
                    if !outcome.emitted_output
                        && error.is_retryable()
                        && attempt < self.recovery.max_attempts
                        && tokio::time::Instant::now() < deadline =>
                {
                    let delay = self.retry_delay(attempt, &error);
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if delay >= remaining {
                        self.record_failure(&error);
                        break Err(to_execution_error(error));
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
        emit_terminal(&sink, request_id, &final_result);
        final_result
    }
}
