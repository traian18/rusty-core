use super::*;
use crate::testing::FakeModelClient;
use harness_model::client::ModelEventSender;

/// A client that advertises `structured_output` must have that reach
/// `BackendCapabilities`. This was wrong once already: the four HTTP
/// integrations set `structured_output: true` on their *factory
/// descriptor* while `GenericModelBackend::new` derived capabilities
/// from `ModelCapabilities`, where the field defaulted to `false` — so
/// every structured-output request would have been rejected by the very
/// backend advertising support for it.
#[test]
fn structured_output_capability_propagates_from_the_client() {
    let backend = GenericModelBackend::new(Arc::new(FakeModelClient::default()));
    assert_eq!(
        backend.capabilities().structured_output,
        FakeModelClient::default().capabilities().structured_output,
        "the backend must mirror its client's structured-output support"
    );
}

/// A constrained request against a backend that cannot honor it must be
/// rejected *before* any network call, rather than silently returning
/// prose the caller will fail to parse much later.
#[tokio::test]
async fn a_response_format_is_rejected_when_the_backend_cannot_honor_it() {
    use harness_protocol::backend::{ExecutionParams, ResponseFormat};

    struct NoStructuredOutput;

    #[async_trait]
    impl ModelClient for NoStructuredOutput {
        fn capabilities(&self) -> harness_model::request::ModelCapabilities {
            harness_model::request::ModelCapabilities {
                streaming: true,
                reasoning: false,
                tool_calls: false,
                parallel_tool_calls: false,
                images: false,
                structured_output: false,
            }
        }

        async fn stream(
            &self,
            _request: ModelRequest,
            _sink: ModelEventSender,
            _cancel: CancellationToken,
        ) -> Result<ModelResult, ModelError> {
            panic!("must be rejected before the client is ever reached");
        }
    }

    let backend = GenericModelBackend::new(Arc::new(NoStructuredOutput));
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
                params: ExecutionParams {
                    response_format: Some(ResponseFormat::JsonObject),
                    ..Default::default()
                },
            },
            sink,
            CancellationToken::new(),
        )
        .await;

    let error = result.expect_err("a constrained request must be rejected");
    assert!(
        matches!(
            &error,
            harness_protocol::backend::ExecutionError::UnsupportedCapability { capability, .. }
                if capability == "structured_output"
        ),
        "expected an UnsupportedCapability(structured_output) error, got: {error:?}"
    );
}

/// `Text` is every provider's default, so it must not require the
/// capability — otherwise a backend without structured output could not
/// serve an ordinary prose request that happened to set the field.
#[tokio::test]
async fn an_explicit_text_format_needs_no_capability() {
    use harness_protocol::backend::{ExecutionParams, ResponseFormat};

    let backend = GenericModelBackend::new(Arc::new(
        FakeModelClient::default()
            .with_capabilities(harness_model::request::ModelCapabilities {
                streaming: true,
                reasoning: false,
                tool_calls: false,
                parallel_tool_calls: false,
                images: false,
                // Explicitly unable to do structured output — the point
                // is that `Text` sails through anyway.
                structured_output: false,
            })
            .with_result(ModelResult {
                stop_reason: "end_turn".to_string(),
                usage: Default::default(),
                cost: Default::default(),
            }),
    ));
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
                params: ExecutionParams {
                    response_format: Some(ResponseFormat::Text),
                    ..Default::default()
                },
            },
            sink,
            CancellationToken::new(),
        )
        .await;
    assert!(
        result.is_ok(),
        "ResponseFormat::Text must not be gated: {result:?}"
    );
}

#[test]
fn recovery_policy_default_allows_long_streamed_answers() {
    let policy = RecoveryPolicy::default();
    assert_eq!(policy.max_attempts, 2);
    assert_eq!(policy.first_event_timeout, Duration::from_secs(180));
    assert_eq!(policy.idle_timeout, Duration::from_secs(120));
    assert_eq!(policy.circuit_failure_threshold, 3);
    assert_eq!(policy.circuit_open_duration, Duration::from_secs(30));
}

#[test]
fn recovery_policy_serde_uses_seconds_and_defaults() {
    let policy: RecoveryPolicy = serde_json::from_value(serde_json::json!({
        "max_attempts": 5,
        "idle_timeout_secs": 45
    }))
    .expect("valid recovery policy");
    assert_eq!(policy.max_attempts, 5);
    assert_eq!(policy.idle_timeout, Duration::from_secs(45));
    // Fields omitted from the JSON fall back to RecoveryPolicy::default().
    assert_eq!(policy.circuit_failure_threshold, 3);
    assert_eq!(policy.circuit_open_duration, Duration::from_secs(30));

    let value = serde_json::to_value(&policy).expect("serializable policy");
    assert_eq!(value["idle_timeout_secs"], 45);
    assert_eq!(value["max_attempts"], 5);
}

#[test]
fn recovery_policy_round_trips_through_json() {
    let policy = RecoveryPolicy {
        max_attempts: 4,
        first_event_timeout: Duration::from_secs(20),
        idle_timeout: Duration::from_secs(20),
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
        first_event_timeout: Duration::from_secs(3),
        idle_timeout: Duration::from_secs(3),
        circuit_failure_threshold: 1,
        circuit_open_duration: Duration::from_secs(2),
    };
    let backend = GenericModelBackend::new_with_recovery(Arc::new(NoopModelClient), custom.clone());
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
            first_event_timeout: Duration::from_secs(2),
            idle_timeout: Duration::from_secs(2),
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
            first_event_timeout: Duration::from_secs(2),
            idle_timeout: Duration::from_secs(2),
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
    let client = FlakyModelClient::new(calls.clone(), 3).with_retry_after(Duration::from_secs(2));
    let backend = GenericModelBackend::new_with_recovery(
        Arc::new(client),
        RecoveryPolicy {
            max_attempts: 5,
            first_event_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(30),
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

/// Structured output must not be resumed by appending a second document.
#[tokio::test]
async fn partial_structured_stream_failure_is_not_retried() {
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
                params: harness_protocol::backend::ExecutionParams {
                    response_format: Some(harness_protocol::backend::ResponseFormat::JsonObject),
                    ..Default::default()
                },
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

#[tokio::test]
async fn interrupted_answers_continue_with_context_without_replaying_tools() {
    use harness_protocol::messages::{ContentBlock, MessageRole};
    struct InterruptedAnswer {
        requests: Mutex<Vec<ModelRequest>>,
        error: ModelError,
        emit_tool: bool,
    }
    #[async_trait]
    impl ModelClient for InterruptedAnswer {
        fn capabilities(&self) -> harness_model::request::ModelCapabilities {
            FakeModelClient::new().capabilities()
        }
        async fn stream(
            &self,
            request: ModelRequest,
            sink: ModelEventSender,
            _cancel: CancellationToken,
        ) -> Result<ModelResult, ModelError> {
            let first = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request);
                requests.len() == 1
            };
            if first {
                let _ = sink
                    .send(ModelEvent::TextDelta {
                        delta: "Research says: ".into(),
                    })
                    .await;
                if self.emit_tool {
                    let _ = sink
                        .send(ModelEvent::ToolCallCompleted {
                            id: harness_protocol::ids::ToolCallId::new(),
                            name: "search".into(),
                            input: serde_json::json!({}),
                        })
                        .await;
                }
                // Exercise the closed-stream return path as real clients
                // need not emit a separate terminal model event.
                return Err(self.error.clone());
            }
            let _ = sink
                .send(ModelEvent::TextDelta {
                    delta: "the answer.".into(),
                })
                .await;
            Ok(ModelResult {
                stop_reason: "stop".into(),
                usage: Default::default(),
                cost: Default::default(),
            })
        }
    }
    for error in [
        ModelError::Timeout,
        ModelError::StreamInterrupted {
            message: "disconnected".into(),
        },
    ] {
        for emit_tool in [false, true] {
            let client = Arc::new(InterruptedAnswer {
                requests: Mutex::new(Vec::new()),
                error: error.clone(),
                emit_tool,
            });
            let backend = GenericModelBackend::new(client.clone());
            let (sink, mut rx) = broadcast::channel(32);
            let result = backend
                .execute(
                    harness_protocol::backend::ExecutionRequest {
                        request_id: RequestId::new(),
                        run_id: harness_protocol::ids::RunId::new(),
                        system_prompt: "Use the research already provided".into(),
                        messages: vec![],
                        tools: vec![harness_protocol::tools::ToolDescriptor {
                            id: harness_protocol::ids::ToolId::new(),
                            name: "search".into(),
                            description: "Search".into(),
                            input_schema: serde_json::json!({}),
                        }],
                        extended_thinking: false,
                        params: Default::default(),
                    },
                    sink,
                    CancellationToken::new(),
                )
                .await;
            let requests = client.requests.lock().unwrap();
            if emit_tool {
                assert!(result.is_err());
                assert_eq!(
                    requests.len(),
                    1,
                    "never replay a turn that dispatched tools"
                );
                continue;
            }
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(requests.len(), 2);
            assert!(requests[1].tools.is_empty());
            assert_eq!(requests[1].system_prompt, requests[0].system_prompt);
            assert_eq!(requests[1].messages[0].role, MessageRole::Assistant);
            assert!(
                matches!(&requests[1].messages[0].content[0], ContentBlock::Text { text } if text == "Research says: ")
            );
            let mut text = String::new();
            let mut completed = 0;
            while let Ok(event) = rx.try_recv() {
                match event {
                    ExecutionEvent::TextDelta { delta, .. } => text.push_str(&delta),
                    ExecutionEvent::Error { error, .. } => {
                        panic!("recovered error leaked: {error:?}")
                    }
                    ExecutionEvent::Completed { .. } => completed += 1,
                    _ => {}
                }
            }
            assert_eq!(text, "Research says: the answer.");
            assert_eq!(completed, 1);
        }
    }
}

/// One HTTP chunk can carry hundreds of SSE frames -- a proxy flushing
/// a buffered upstream all at once -- and a client parses and emits them
/// with no await in between. Over the previous `broadcast` channel the
/// receiver then saw `Lagged(n)` and the attempt failed with
/// `EVENT_LAG` ("lost 76 model events"). The bounded channel makes the
/// client wait instead, so every delta reaches the sink.
#[tokio::test]
async fn a_synchronous_burst_of_model_events_is_delivered_in_full() {
    struct Burst {
        count: usize,
    }
    #[async_trait]
    impl ModelClient for Burst {
        fn capabilities(&self) -> harness_model::request::ModelCapabilities {
            FakeModelClient::new().capabilities()
        }
        async fn stream(
            &self,
            _request: ModelRequest,
            sink: ModelEventSender,
            cancel: CancellationToken,
        ) -> Result<ModelResult, ModelError> {
            for index in 0..self.count {
                harness_model::send_event(
                    &sink,
                    ModelEvent::TextDelta {
                        delta: format!("{index} "),
                    },
                    &cancel,
                )
                .await?;
            }
            let result = ModelResult {
                stop_reason: "stop".into(),
                usage: Default::default(),
                cost: Default::default(),
            };
            harness_model::send_event(
                &sink,
                ModelEvent::Completed {
                    result: result.clone(),
                },
                &cancel,
            )
            .await?;
            Ok(result)
        }
    }

    let count = MODEL_EVENT_BUFFER * 8;
    let backend = GenericModelBackend::new(Arc::new(Burst { count }));
    let (sink, mut rx) = broadcast::channel(count + 16);
    let result = backend
        .execute(
            harness_protocol::backend::ExecutionRequest {
                request_id: RequestId::new(),
                run_id: harness_protocol::ids::RunId::new(),
                system_prompt: String::new(),
                messages: vec![],
                tools: vec![],
                extended_thinking: false,
                params: Default::default(),
            },
            sink,
            CancellationToken::new(),
        )
        .await;
    assert!(
        result.is_ok(),
        "burst must not fail the attempt: {result:?}"
    );

    let mut deltas = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let ExecutionEvent::TextDelta { delta, .. } = event {
            deltas.push(delta);
        }
    }
    assert_eq!(deltas.len(), count, "every delta must be forwarded");
    assert!(
        deltas
            .iter()
            .enumerate()
            .all(|(index, delta)| delta == &format!("{index} ")),
        "deltas must keep their order"
    );
}

#[tokio::test(start_paused = true)]
async fn first_event_has_its_own_longer_timeout() {
    struct SlowStart;
    #[async_trait]
    impl ModelClient for SlowStart {
        fn capabilities(&self) -> harness_model::request::ModelCapabilities {
            FakeModelClient::new().capabilities()
        }
        async fn stream(
            &self,
            _request: ModelRequest,
            sink: ModelEventSender,
            _cancel: CancellationToken,
        ) -> Result<ModelResult, ModelError> {
            tokio::time::sleep(Duration::from_secs(15)).await;
            let _ = sink
                .send(ModelEvent::TextDelta {
                    delta: "text".into(),
                })
                .await;
            Ok(ModelResult {
                stop_reason: "stop".into(),
                usage: Default::default(),
                cost: Default::default(),
            })
        }
    }
    for (first_event, expect_timeout) in [(20, false), (10, true)] {
        let backend = GenericModelBackend::new_with_recovery(
            Arc::new(SlowStart),
            RecoveryPolicy {
                first_event_timeout: Duration::from_secs(first_event),
                idle_timeout: Duration::from_secs(5),
                max_attempts: 1,
                ..Default::default()
            },
        );
        let (sink, _rx) = broadcast::channel(32);
        let result = backend
            .execute(
                harness_protocol::backend::ExecutionRequest {
                    request_id: RequestId::new(),
                    run_id: harness_protocol::ids::RunId::new(),
                    system_prompt: String::new(),
                    messages: vec![],
                    tools: vec![],
                    extended_thinking: false,
                    params: Default::default(),
                },
                sink,
                CancellationToken::new(),
            )
            .await;
        if expect_timeout {
            assert!(matches!(result, Err(ExecutionError::Timeout)), "{result:?}");
        } else {
            assert!(result.is_ok(), "{result:?}");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn timeout_tracks_inactivity_instead_of_stream_duration() {
    struct SlowStream {
        stall: bool,
    }
    #[async_trait]
    impl ModelClient for SlowStream {
        fn capabilities(&self) -> harness_model::request::ModelCapabilities {
            FakeModelClient::new().capabilities()
        }
        async fn stream(
            &self,
            _request: ModelRequest,
            sink: ModelEventSender,
            cancel: CancellationToken,
        ) -> Result<ModelResult, ModelError> {
            for _ in 0..4 {
                tokio::time::sleep(Duration::from_secs(6)).await;
                let _ = sink
                    .send(ModelEvent::TextDelta {
                        delta: "text ".into(),
                    })
                    .await;
            }
            if self.stall {
                cancel.cancelled().await;
                return Err(ModelError::Cancelled);
            }
            Ok(ModelResult {
                stop_reason: "stop".into(),
                usage: Default::default(),
                cost: Default::default(),
            })
        }
    }
    for stall in [false, true] {
        let backend = GenericModelBackend::new_with_recovery(
            Arc::new(SlowStream { stall }),
            RecoveryPolicy {
                first_event_timeout: Duration::from_secs(10),
                idle_timeout: Duration::from_secs(10),
                max_attempts: 1,
                ..Default::default()
            },
        );
        let start = tokio::time::Instant::now();
        let (sink, _rx) = broadcast::channel(32);
        let result = backend
            .execute(
                harness_protocol::backend::ExecutionRequest {
                    request_id: RequestId::new(),
                    run_id: harness_protocol::ids::RunId::new(),
                    system_prompt: String::new(),
                    messages: vec![],
                    tools: vec![],
                    extended_thinking: false,
                    params: Default::default(),
                },
                sink,
                CancellationToken::new(),
            )
            .await;
        if stall {
            assert!(matches!(result, Err(ExecutionError::Timeout)));
            assert_eq!(start.elapsed(), Duration::from_secs(34));
        } else {
            assert!(
                result.is_ok(),
                "active stream must outlive the idle limit: {result:?}"
            );
            assert_eq!(start.elapsed(), Duration::from_secs(24));
        }
    }
}

mod harness_generic_backend_test_support {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use tokio_util::sync::CancellationToken;

    use harness_model::client::{ModelClient, ModelEventSender};
    use harness_model::events::{ModelError, ModelResult};
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
                structured_output: false,
            }
        }

        async fn stream(
            &self,
            _request: ModelRequest,
            _sink: ModelEventSender,
            _cancel: CancellationToken,
        ) -> Result<harness_model::events::ModelResult, harness_model::events::ModelError> {
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
                structured_output: false,
            }
        }

        async fn stream(
            &self,
            _request: ModelRequest,
            _sink: ModelEventSender,
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
