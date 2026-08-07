//! Provider-neutral `ModelClient` adapter for the runtime `ExecutionBackend`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
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
    /// Deadline shared by all attempts and backoff delays.
    #[serde(
        rename = "total_deadline_secs",
        serialize_with = "serialize_duration_secs",
        deserialize_with = "deserialize_duration_secs"
    )]
    pub total_deadline: Duration,
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
            images: model.images,
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
        deadline: tokio::time::Instant,
    ) -> AttemptOutcome {
        let (model_tx, mut model_rx) = broadcast::channel(256);
        let client = self.model_client.clone();
        let attempt_cancel = cancel.child_token();
        let task_cancel = attempt_cancel.clone();
        #[allow(unused_mut)]
        let mut stream =
            tokio::spawn(async move { client.stream(model_request, model_tx, task_cancel).await });
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
        finish_reason: result.stop_reason,
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
        let model_request = ModelRequest {
            system_prompt: request.system_prompt,
            messages: request.messages,
            tools: request.tools,
            model: request.params.model,
            max_tokens: request.params.max_tokens,
            temperature: request.params.temperature,
            stop_sequences: request.params.stop_sequences,
            extended_thinking: request.extended_thinking,
            reasoning_effort: request.params.reasoning_effort,
            provider_options: request.params.provider_options,
        };
        if let Err(error) = self.circuit_allows_request() {
            metrics::counter!("harness_backend_requests_total", "backend" => backend_label.clone(), "outcome" => "rejected_circuit_open").increment(1);
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
mod tests {
    use super::*;
    use crate::testing::FakeModelClient;

    #[test]
    fn recovery_policy_default_matches_prior_hardcoded_values() {
        let policy = RecoveryPolicy::default();
        assert_eq!(policy.max_attempts, 2);
        assert_eq!(policy.total_deadline, Duration::from_secs(15));
        assert_eq!(policy.circuit_failure_threshold, 3);
        assert_eq!(policy.circuit_open_duration, Duration::from_secs(30));
    }

    #[test]
    fn recovery_policy_serde_uses_seconds_and_defaults() {
        let policy: RecoveryPolicy = serde_json::from_value(serde_json::json!({
            "max_attempts": 5,
            "total_deadline_secs": 45
        }))
        .expect("valid recovery policy");
        assert_eq!(policy.max_attempts, 5);
        assert_eq!(policy.total_deadline, Duration::from_secs(45));
        // Fields omitted from the JSON fall back to RecoveryPolicy::default().
        assert_eq!(policy.circuit_failure_threshold, 3);
        assert_eq!(policy.circuit_open_duration, Duration::from_secs(30));

        let value = serde_json::to_value(&policy).expect("serializable policy");
        assert_eq!(value["total_deadline_secs"], 45);
        assert_eq!(value["max_attempts"], 5);
    }

    #[test]
    fn recovery_policy_round_trips_through_json() {
        let policy = RecoveryPolicy {
            max_attempts: 4,
            total_deadline: Duration::from_secs(20),
            circuit_failure_threshold: 7,
            circuit_open_duration: Duration::from_secs(60),
        };
        let json = serde_json::to_string(&policy).expect("serialize");
        let deserialized: RecoveryPolicy = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized, policy);
    }

    #[test]
    fn new_with_recovery_threads_the_custom_policy_through() {
        use harness_generic_backend_test_support::NoopModelClient;

        let custom = RecoveryPolicy {
            max_attempts: 9,
            total_deadline: Duration::from_secs(3),
            circuit_failure_threshold: 1,
            circuit_open_duration: Duration::from_secs(2),
        };
        let backend =
            GenericModelBackend::new_with_recovery(Arc::new(NoopModelClient), custom.clone());
        assert_eq!(backend.recovery_policy(), &custom);
    }

    #[test]
    fn new_uses_default_recovery_policy() {
        use harness_generic_backend_test_support::NoopModelClient;

        let backend = GenericModelBackend::new(Arc::new(NoopModelClient));
        assert_eq!(backend.recovery_policy(), &RecoveryPolicy::default());
    }

    #[tokio::test]
    async fn retries_transient_failures_up_to_configured_attempts() {
        use harness_generic_backend_test_support::FlakyModelClient;

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let client = FlakyModelClient::new(calls.clone(), 3);
        let backend = GenericModelBackend::new_with_recovery(
            Arc::new(client),
            RecoveryPolicy {
                max_attempts: 3,
                total_deadline: Duration::from_secs(2),
                ..RecoveryPolicy::default()
            },
        );
        let (sink, _rx) = broadcast::channel(16);
        let result = backend
            .execute(
                harness_protocol::backend::ExecutionRequest {
                    request_id: RequestId::new(),
                    run_id: harness_protocol::ids::RunId::new(),
                    system_prompt: String::new(),
                    messages: Vec::new(),
                    tools: Vec::new(),
                    extended_thinking: false,
                    params: Default::default(),
                },
                sink,
                CancellationToken::new(),
            )
            .await;

        assert!(result.is_ok());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn stops_retrying_at_configured_attempt_limit() {
        use harness_generic_backend_test_support::FlakyModelClient;

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let client = FlakyModelClient::new(calls.clone(), 3);
        let backend = GenericModelBackend::new_with_recovery(
            Arc::new(client),
            RecoveryPolicy {
                max_attempts: 2,
                total_deadline: Duration::from_secs(2),
                ..RecoveryPolicy::default()
            },
        );
        let (sink, _rx) = broadcast::channel(16);
        let result = backend
            .execute(
                harness_protocol::backend::ExecutionRequest {
                    request_id: RequestId::new(),
                    run_id: harness_protocol::ids::RunId::new(),
                    system_prompt: String::new(),
                    messages: Vec::new(),
                    tools: Vec::new(),
                    extended_thinking: false,
                    params: Default::default(),
                },
                sink,
                CancellationToken::new(),
            )
            .await;

        assert!(matches!(result, Err(ExecutionError::RateLimited { .. })));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// M2: "cancel during retry delay" — the race the roadmap's M2 section
    /// long listed as blocked on a retry mechanism that didn't exist yet.
    /// It exists (this file's own `retry_delay`/backoff `tokio::select!`
    /// against `cancel.cancelled()`); this test proves cancellation
    /// firing *during the sleep between attempts* wins the race, returning
    /// `ExecutionError::Cancelled` promptly rather than waiting out the
    /// full delay or letting a queued retry fire after cancellation.
    #[tokio::test]
    async fn cancel_wins_the_race_against_a_retry_backoff_delay() {
        use harness_generic_backend_test_support::FlakyModelClient;

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // succeed_on: 3 means the first two calls fail retryable; a 2s
        // retry_after gives ample time to fire cancellation mid-sleep
        // without racing against real-world scheduling jitter.
        let client =
            FlakyModelClient::new(calls.clone(), 3).with_retry_after(Duration::from_secs(2));
        let backend = GenericModelBackend::new_with_recovery(
            Arc::new(client),
            RecoveryPolicy {
                max_attempts: 5,
                total_deadline: Duration::from_secs(30),
                ..RecoveryPolicy::default()
            },
        );
        let (sink, _rx) = broadcast::channel(16);
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();

        let handle = tokio::spawn(async move {
            backend
                .execute(
                    harness_protocol::backend::ExecutionRequest {
                        request_id: RequestId::new(),
                        run_id: harness_protocol::ids::RunId::new(),
                        system_prompt: String::new(),
                        messages: Vec::new(),
                        tools: Vec::new(),
                        extended_thinking: false,
                        params: Default::default(),
                    },
                    sink,
                    cancel_for_task,
                )
                .await
        });

        // Let the first attempt fail and enter its retry sleep, then cancel
        // well before the 2s retry_after would naturally elapse.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the first attempt should have already failed and be sleeping before cancelling"
        );
        cancel.cancel();

        let result = tokio::time::timeout(Duration::from_millis(500), handle)
            .await
            .expect("execute() must return promptly once cancelled mid-retry-delay, not after the full 2s backoff")
            .expect("task must not panic");

        assert!(
            matches!(result, Err(ExecutionError::Cancelled)),
            "expected Cancelled, got {result:?}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "cancellation during the retry delay must prevent the second attempt from ever firing"
        );
    }

    /// M2: provider partial-stream failure — an error arriving *after* some
    /// deltas were already streamed, not just before any output at all.
    /// This matters because `GenericModelBackend`'s retry logic explicitly
    /// only retries when `!outcome.emitted_output` (see `execute`) — a
    /// failure after partial output must NOT be silently retried (that
    /// would replay/duplicate the already-emitted deltas to the caller); it
    /// must propagate as a terminal error on the first such failure.
    #[tokio::test]
    async fn partial_stream_failure_after_some_deltas_is_not_retried() {
        let client = FakeModelClient::new()
            .with_events(vec![
                ModelEvent::TextDelta {
                    delta: "partial ".to_string(),
                },
                ModelEvent::TextDelta {
                    delta: "output".to_string(),
                },
            ])
            .with_error(ModelError::BackendError {
                message: "connection reset mid-stream".to_string(),
                code: "503".to_string(),
            });
        let backend = GenericModelBackend::new_with_recovery(
            Arc::new(client),
            RecoveryPolicy {
                max_attempts: 5,
                ..RecoveryPolicy::default()
            },
        );
        let (sink, mut rx) = broadcast::channel(16);
        let result = backend
            .execute(
                harness_protocol::backend::ExecutionRequest {
                    request_id: RequestId::new(),
                    run_id: harness_protocol::ids::RunId::new(),
                    system_prompt: String::new(),
                    messages: Vec::new(),
                    tools: Vec::new(),
                    extended_thinking: false,
                    params: Default::default(),
                },
                sink,
                CancellationToken::new(),
            )
            .await;

        // 503 is normally retryable, but not after output was already
        // emitted — the terminal error must surface on the very first
        // attempt rather than silently retrying and re-emitting deltas.
        assert!(
            matches!(result, Err(ExecutionError::BackendError { .. })),
            "expected the partial-stream failure to surface directly, got {result:?}"
        );

        let mut delta_count = 0;
        let mut saw_error = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                ExecutionEvent::TextDelta { .. } => delta_count += 1,
                ExecutionEvent::Error { .. } => saw_error = true,
                _ => {}
            }
        }
        assert_eq!(
            delta_count, 2,
            "both deltas emitted before the failure must have reached the sink exactly once"
        );
        assert!(saw_error, "the terminal error must also reach the sink");
    }

    mod harness_generic_backend_test_support {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        use async_trait::async_trait;
        use tokio::sync::broadcast;
        use tokio_util::sync::CancellationToken;

        use harness_model::client::ModelClient;
        use harness_model::events::{ModelError, ModelEvent, ModelResult};
        use harness_model::request::{ModelCapabilities, ModelRequest};

        /// Minimal `ModelClient` used only to construct a `GenericModelBackend`
        /// for policy-plumbing assertions — never actually streamed against.
        pub struct NoopModelClient;

        #[async_trait]
        impl ModelClient for NoopModelClient {
            fn capabilities(&self) -> ModelCapabilities {
                ModelCapabilities {
                    streaming: true,
                    reasoning: false,
                    tool_calls: false,
                    parallel_tool_calls: false,
                    images: false,
                }
            }

            async fn stream(
                &self,
                _request: ModelRequest,
                _sink: broadcast::Sender<ModelEvent>,
                _cancel: CancellationToken,
            ) -> Result<harness_model::events::ModelResult, harness_model::events::ModelError>
            {
                unreachable!("NoopModelClient is never streamed against in these tests")
            }
        }

        pub struct FlakyModelClient {
            calls: Arc<AtomicUsize>,
            succeed_on: usize,
            retry_after: Duration,
        }

        impl FlakyModelClient {
            pub fn new(calls: Arc<AtomicUsize>, succeed_on: usize) -> Self {
                Self {
                    calls,
                    succeed_on,
                    retry_after: Duration::ZERO,
                }
            }

            /// M2: configurable retry delay, so a test can cancel mid-sleep
            /// (a zero delay resolves too fast to race against). Used by
            /// `cancel_wins_the_race_against_a_retry_backoff_delay`.
            pub fn with_retry_after(mut self, retry_after: Duration) -> Self {
                self.retry_after = retry_after;
                self
            }
        }

        #[async_trait]
        impl ModelClient for FlakyModelClient {
            fn capabilities(&self) -> ModelCapabilities {
                ModelCapabilities {
                    streaming: true,
                    reasoning: false,
                    tool_calls: false,
                    parallel_tool_calls: false,
                    images: false,
                }
            }

            async fn stream(
                &self,
                _request: ModelRequest,
                _sink: broadcast::Sender<ModelEvent>,
                _cancel: CancellationToken,
            ) -> Result<ModelResult, ModelError> {
                let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
                if attempt < self.succeed_on {
                    Err(ModelError::RateLimited {
                        retry_after: Some(self.retry_after),
                    })
                } else {
                    Ok(ModelResult {
                        stop_reason: "end_turn".to_string(),
                        usage: Default::default(),
                        cost: Default::default(),
                    })
                }
            }
        }
    }
}
