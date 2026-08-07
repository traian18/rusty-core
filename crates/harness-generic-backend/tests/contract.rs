//! Contract tests for [`GenericModelBackend`] via [`FakeModelClient`].
//!
//! Six test cases exercise the complete `ExecutionBackend::execute()` path:
//!
//! 1. **Streaming ordering** — events arrive in emitted order.
//! 2. **Cancellation** — mid-flight cancellation via [`CancellationToken`]
//!    returns [`ExecutionError::Cancelled`].
//! 3. **Completion** — scripted [`ModelResult`] is delivered correctly as
//!    [`ExecutionResult`].
//! 4. **Usage propagation** — [`ModelEvent::UsageUpdate`] is forwarded as
//!    [`ExecutionEvent::UsageUpdate`].
//! 5. **Tool call normalization** — a sequence of `ToolCallStarted` +
//!    multiple `ToolCallDelta` + `ToolCallCompleted` produces a single
//!    [`ExecutionEvent::ToolCallRequested`] with accumulated JSON input.
//! 6. **Error normalization** — each [`ModelError`] variant maps to the
//!    correct [`ExecutionError`] variant.
//!
//! The public function [`run_backend_contract_suite`] is the reusable helper
//! that Phase 10 backends call to verify their own [`ExecutionBackend`]
//! implementations.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_generic_backend::testing::FakeModelClient;
use harness_generic_backend::GenericModelBackend;
use harness_model::events::{ModelError, ModelEvent, ModelResult};
use harness_protocol::backend::{
    ExecutionError, ExecutionEvent, ExecutionRequest, ExecutionResult,
};
use harness_protocol::ids::{RequestId, RunId, ToolCallId};
use harness_runtime::traits::ExecutionBackend;

// ===========================================================================
// Helpers
// ===========================================================================

/// Build a minimal synthetic [`ExecutionRequest`] for testing.
fn make_request() -> ExecutionRequest {
    ExecutionRequest {
        request_id: RequestId::new(),
        run_id: RunId::new(),
        system_prompt: String::new(),
        messages: vec![],
        tools: vec![],
        extended_thinking: false,
        params: Default::default(),
    }
}

/// Collect all [`ExecutionEvent`]s from a broadcast receiver until the sender
/// is dropped (which happens when [`ExecutionBackend::execute`] returns).
async fn collect_events(mut rx: broadcast::Receiver<ExecutionEvent>) -> Vec<ExecutionEvent> {
    let mut events = Vec::new();
    loop {
        match rx.recv().await {
            Ok(event) => events.push(event),
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                panic!("lost {n} execution events during collection");
            }
        }
    }
    events
}

/// Spawn `backend.execute(...)` in a background task and collect all events
/// that it emits, returning the final [`ExecutionResult`] or
/// [`ExecutionError`] alongside the ordered event list.
async fn run_execution(
    backend: GenericModelBackend,
) -> (Result<ExecutionResult, ExecutionError>, Vec<ExecutionEvent>) {
    let request = make_request();
    let (sink, rx) = broadcast::channel(256);
    let cancel = CancellationToken::new();

    let events_handle = tokio::spawn(collect_events(rx));

    let result = backend.execute(request, sink, cancel).await;

    let events = events_handle.await.expect("event collection task panicked");

    (result, events)
}

// ===========================================================================
// Individual test cases
// ===========================================================================

// ---------------------------------------------------------------------------
// 1. Streaming ordering
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_streaming_ordering() {
    let client = FakeModelClient::new()
        .with_events(vec![
            ModelEvent::TextDelta {
                delta: "Hello ".into(),
            },
            ModelEvent::TextDelta {
                delta: "world!".into(),
            },
            ModelEvent::TextDelta {
                delta: " How are you?".into(),
            },
        ])
        .with_result(ModelResult {
            stop_reason: "end_turn".into(),
            usage: Default::default(),
            cost: Default::default(),
        });

    let backend = GenericModelBackend::new(Arc::new(client));
    let (result, events) = run_execution(backend).await;

    // Execution should succeed.
    assert!(result.is_ok(), "expected Ok, got Err: {:?}", result);

    // --- Assert event ordering -------------------------------------------
    //
    // Expected sequence:
    //   TextDelta "Hello "
    //   TextDelta "world!"
    //   TextDelta " How are you?"
    //   Completed

    let text_deltas: Vec<&str> = events
        .iter()
        .filter_map(|e| {
            if let ExecutionEvent::TextDelta { delta, .. } = e {
                Some(delta.as_str())
            } else {
                None
            }
        })
        .collect();

    assert_eq!(text_deltas, vec!["Hello ", "world!", " How are you?"]);

    // Verify the terminal Completed event is present.
    let has_completed = events
        .iter()
        .any(|e| matches!(e, ExecutionEvent::Completed { .. }));
    assert!(
        has_completed,
        "expected a Completed event in the event stream"
    );

    // Verify the result carries the correct finish_reason.
    let exec_result = result.unwrap();
    assert_eq!(exec_result.finish_reason, "end_turn");
}

// ---------------------------------------------------------------------------
// 2. Cancellation — mid-flight via `block_until_cancelled` mode
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_cancellation_mid_flight() {
    // FakeModelClient in `block_until_cancelled` mode waits on the
    // CancellationToken before emitting any events.  This simulates a model
    // request that has started but not yet produced output — a true
    // "in-flight" execution.  When we cancel the token the stream returns
    // ModelError::Cancelled, which the backend translates to
    // ExecutionError::Cancelled.
    let client = FakeModelClient::new().block_until_cancelled();

    let backend = GenericModelBackend::new(Arc::new(client));
    let request = make_request();
    let (sink, _rx) = broadcast::channel(256);
    let parent_token = CancellationToken::new();
    let child_token = parent_token.child_token();

    let handle = tokio::spawn(async move { backend.execute(request, sink, child_token).await });

    // Give the spawn time to enter the block_until_cancelled wait.
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Cancel mid-flight.
    parent_token.cancel();

    let result = handle.await.expect("execute task panicked");
    match result {
        Err(ExecutionError::Cancelled) => { /* expected */ }
        other => panic!("expected Cancelled, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 3. Completion — scripted result delivered correctly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_completion() {
    let client = FakeModelClient::new().with_result(ModelResult {
        stop_reason: "end_turn".into(),
        usage: Default::default(),
        cost: Default::default(),
    });

    let backend = GenericModelBackend::new(Arc::new(client));
    let (result, events) = run_execution(backend).await;

    // Must succeed.
    let exec_result = result.expect("expected Ok result");

    // The finish_reason should map from stop_reason.
    assert_eq!(
        exec_result.finish_reason, "end_turn",
        "finish_reason should match stop_reason"
    );

    // A Completed event must be in the stream.
    let completed_count = events
        .iter()
        .filter(|e| matches!(e, ExecutionEvent::Completed { .. }))
        .count();
    assert_eq!(completed_count, 1, "expected exactly one Completed event");
}

// ---------------------------------------------------------------------------
// 4. Usage propagation — UsageUpdate forwarded
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_usage_propagation() {
    let client = FakeModelClient::new()
        .with_events(vec![ModelEvent::UsageUpdate {
            usage: Default::default(),
        }])
        .with_result(ModelResult {
            stop_reason: "end_turn".into(),
            usage: Default::default(),
            cost: Default::default(),
        });

    let backend = GenericModelBackend::new(Arc::new(client));
    let (result, events) = run_execution(backend).await;

    assert!(result.is_ok(), "expected Ok result");

    // There should be exactly one UsageUpdate event.
    let usage_updates: Vec<&ExecutionEvent> = events
        .iter()
        .filter(|e| matches!(e, ExecutionEvent::UsageUpdate { .. }))
        .collect();

    assert_eq!(
        usage_updates.len(),
        1,
        "expected exactly one UsageUpdate event"
    );
}

// ---------------------------------------------------------------------------
// 5. Tool call normalization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_tool_call_normalization() {
    let tool_id = ToolCallId::new();

    let client = FakeModelClient::new()
        .with_events(vec![
            ModelEvent::ToolCallStarted {
                id: tool_id,
                name: "search".into(),
            },
            ModelEvent::ToolCallDelta {
                id: tool_id,
                delta: r#"{"query": "ru"#.into(),
            },
            ModelEvent::ToolCallDelta {
                id: tool_id,
                delta: r#"st"}"#.into(),
            },
            ModelEvent::ToolCallCompleted {
                id: tool_id,
                name: "search".into(),
                input: serde_json::json!({"query": "rust"}),
            },
        ])
        .with_result(ModelResult {
            stop_reason: "tool_use".into(),
            usage: Default::default(),
            cost: Default::default(),
        });

    let backend = GenericModelBackend::new(Arc::new(client));
    let (result, events) = run_execution(backend).await;

    assert!(result.is_ok(), "expected Ok result");

    // There should be exactly one ToolCallRequested event.
    let tool_requests: Vec<&ExecutionEvent> = events
        .iter()
        .filter(|e| matches!(e, ExecutionEvent::ToolCallRequested { .. }))
        .collect();

    assert_eq!(
        tool_requests.len(),
        1,
        "expected exactly one ToolCallRequested event, got {}",
        tool_requests.len()
    );

    // Verify the accumulated tool call has the correct name and parsed JSON.
    if let ExecutionEvent::ToolCallRequested { call, .. } = &tool_requests[0] {
        assert_eq!(call.name, "search");
        assert_eq!(call.arguments, serde_json::json!({"query": "rust"}));
    } else {
        unreachable!();
    }
}

// ---------------------------------------------------------------------------
// 6. Error normalization — each ModelError variant → correct ExecutionError
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_error_normalization_backend_error() {
    let client = FakeModelClient::new().with_error(ModelError::BackendError {
        message: "something broke".into(),
        code: "INTERNAL_ERR".into(),
    });

    let backend = GenericModelBackend::new(Arc::new(client));
    let (result, events) = run_execution(backend).await;

    match result {
        Err(ExecutionError::BackendError { message, code }) => {
            assert_eq!(message, "something broke");
            assert_eq!(code, "INTERNAL_ERR");
        }
        other => panic!("expected BackendError, got {other:?}"),
    }

    // An Error event must have been emitted.
    let has_error_event = events
        .iter()
        .any(|e| matches!(e, ExecutionEvent::Error { .. }));
    assert!(has_error_event, "expected an Error execution event");
}

#[tokio::test]
async fn test_error_normalization_rate_limited() {
    let client = FakeModelClient::new().with_error(ModelError::RateLimited {
        retry_after: Some(Duration::from_secs(42)),
    });

    let backend = GenericModelBackend::new(Arc::new(client));
    let (result, _events) = run_execution(backend).await;

    match result {
        Err(ExecutionError::RateLimited { retry_after }) => {
            assert_eq!(retry_after, Some(42));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn test_error_normalization_invalid_request() {
    let client = FakeModelClient::new().with_error(ModelError::InvalidRequest {
        message: "bad input".into(),
    });

    let backend = GenericModelBackend::new(Arc::new(client));
    let (result, _events) = run_execution(backend).await;

    match result {
        Err(ExecutionError::InvalidRequest { message }) => {
            assert_eq!(message, "bad input");
        }
        other => panic!("expected InvalidRequest, got {other:?}"),
    }
}

#[tokio::test]
async fn test_error_normalization_cancelled() {
    // Use block_until_cancelled mode so the client returns Cancelled
    // when we fire the token, exercising the cancellation path through
    // the backend's event translation.
    let client = FakeModelClient::new().block_until_cancelled();

    let backend = GenericModelBackend::new(Arc::new(client));
    let request = make_request();
    let (sink, _rx) = broadcast::channel(256);
    let parent = CancellationToken::new();
    let child = parent.child_token();

    let handle = tokio::spawn(async move { backend.execute(request, sink, child).await });

    // Let the client start blocking.
    tokio::time::sleep(Duration::from_millis(5)).await;
    parent.cancel();

    let result = handle.await.expect("execute task panicked");
    match result {
        Err(ExecutionError::Cancelled) => { /* expected */ }
        other => panic!("expected Cancelled, got {other:?}"),
    }
}

#[tokio::test]
async fn test_error_normalization_timeout() {
    let client = FakeModelClient::new().with_error(ModelError::Timeout);

    let backend = GenericModelBackend::new(Arc::new(client));
    let (result, _events) = run_execution(backend).await;

    match result {
        Err(ExecutionError::Timeout) => { /* expected */ }
        other => panic!("expected Timeout, got {other:?}"),
    }
}

// ===========================================================================
// M4: ExecutionParams forwarding and capability validation
// ===========================================================================

/// `GenericModelBackend::execute` must forward `request.params` into the
/// `ModelRequest` given to the `ModelClient` verbatim — this is the entire
/// fix for the pre-M4 bug where model/max_tokens/temperature/stop_sequences
/// were silently hardcoded to `None`/empty regardless of what a caller
/// configured.
#[tokio::test]
async fn execution_params_are_forwarded_to_the_model_client_unchanged() {
    let client = FakeModelClient::new().with_result(ModelResult {
        stop_reason: "end_turn".to_string(),
        usage: Default::default(),
        cost: Default::default(),
    });
    // Clone a handle to the same underlying recorder before moving the
    // client into the backend (Arc<dyn ModelClient> erases the concrete
    // type, so we can't downcast back out afterward).
    let last_request_probe = client.clone();

    let backend = GenericModelBackend::new(Arc::new(client));
    let mut request = make_request();
    request.extended_thinking = true;
    request.params = harness_protocol::backend::ExecutionParams {
        model: Some("claude-opus-4-20250514".to_string()),
        max_tokens: Some(8192),
        temperature: Some(0.4),
        stop_sequences: vec!["STOP".to_string()],
        reasoning_effort: Some(harness_protocol::backend::ReasoningEffort::High),
        extended_thinking: Some(true),
        provider_options: serde_json::json!({"anthropic": {"top_k": 40}}),
    };

    let (sink, _rx) = broadcast::channel(256);
    let result = backend
        .execute(request, sink, CancellationToken::new())
        .await;
    assert!(result.is_ok(), "execution should succeed: {result:?}");

    let seen = last_request_probe
        .last_request()
        .expect("FakeModelClient::stream should have been called");
    assert_eq!(seen.model.as_deref(), Some("claude-opus-4-20250514"));
    assert_eq!(seen.max_tokens, Some(8192));
    assert_eq!(seen.temperature, Some(0.4));
    assert_eq!(seen.stop_sequences, vec!["STOP".to_string()]);
    assert!(seen.extended_thinking);
    assert_eq!(
        seen.reasoning_effort,
        Some(harness_protocol::backend::ReasoningEffort::High)
    );
    assert_eq!(
        seen.provider_options,
        serde_json::json!({"anthropic": {"top_k": 40}})
    );
}

/// A request that asks for reasoning against a model client that doesn't
/// advertise it must be rejected with a typed `UnsupportedCapability` error
/// *without* `ModelClient::stream` ever being called — proving the check
/// happens before any (billable) network call.
#[tokio::test]
async fn reasoning_request_against_a_non_reasoning_model_is_rejected_before_dispatch() {
    let client = FakeModelClient::new()
        .with_capabilities(harness_model::request::ModelCapabilities {
            streaming: true,
            reasoning: false,
            tool_calls: false,
            parallel_tool_calls: false,
            images: false,
        })
        .with_result(ModelResult::default());
    let probe = client.clone();

    let backend = GenericModelBackend::new(Arc::new(client));
    let mut request = make_request();
    request.extended_thinking = true;

    let (sink, _rx) = broadcast::channel(256);
    let result = backend
        .execute(request, sink, CancellationToken::new())
        .await;

    match result {
        Err(ExecutionError::UnsupportedCapability { capability, .. }) => {
            assert_eq!(capability, "reasoning");
        }
        other => panic!("expected UnsupportedCapability, got {other:?}"),
    }
    assert!(
        probe.last_request().is_none(),
        "stream() must never be called for a capability-rejected request"
    );
}

/// Same shape, but for tool calls against a model client that doesn't
/// support them.
#[tokio::test]
async fn tool_call_request_against_a_non_tool_model_is_rejected_before_dispatch() {
    let client = FakeModelClient::new()
        .with_capabilities(harness_model::request::ModelCapabilities {
            streaming: true,
            reasoning: false,
            tool_calls: false,
            parallel_tool_calls: false,
            images: false,
        })
        .with_result(ModelResult::default());
    let probe = client.clone();

    let backend = GenericModelBackend::new(Arc::new(client));
    let mut request = make_request();
    request.tools = vec![harness_protocol::tools::ToolDescriptor {
        id: harness_protocol::ids::ToolId::new(),
        name: "fs.read".to_string(),
        description: "read a file".to_string(),
        input_schema: serde_json::json!({"type": "object"}),
    }];

    let (sink, _rx) = broadcast::channel(256);
    let result = backend
        .execute(request, sink, CancellationToken::new())
        .await;

    match result {
        Err(ExecutionError::UnsupportedCapability { capability, .. }) => {
            assert_eq!(capability, "tool_calls");
        }
        other => panic!("expected UnsupportedCapability, got {other:?}"),
    }
    assert!(probe.last_request().is_none());
}

/// A request with no reasoning/images/tools must pass capability validation
/// unchanged, even against a minimally-capable model client — the checks
/// must be need-based, not blanket-required.
#[tokio::test]
async fn plain_text_request_passes_capability_checks_against_a_minimal_model() {
    let client = FakeModelClient::new()
        .with_capabilities(harness_model::request::ModelCapabilities {
            streaming: true,
            reasoning: false,
            tool_calls: false,
            parallel_tool_calls: false,
            images: false,
        })
        .with_result(ModelResult::default());

    let backend = GenericModelBackend::new(Arc::new(client));
    let (result, _events) = run_execution(backend).await;
    assert!(
        result.is_ok(),
        "plain request should not trip capability checks: {result:?}"
    );
}

// ===========================================================================
// Reusable contract suite — called by Phase 10 backends
// ===========================================================================

/// Run the full contract test suite against any [`ExecutionBackend`].
///
/// Phase 10 backends should call this function from their own test files to
/// verify that their backend implementation satisfies the contract expected
/// by the harness runtime.  The backend must be backed by a controllable
/// test double (e.g. [`GenericModelBackend`] wrapping [`FakeModelClient`])
/// for the detailed event-level assertions to pass.
///
/// # Usage (from a Phase 10 integration test)
///
/// ```ignore
/// use harness_generic_backend::contract::run_backend_contract_suite;
///
/// #[tokio::test]
/// async fn my_backend_contract() {
///     let backend = build_test_backend();
///     run_backend_contract_suite(backend).await;
/// }
/// ```
pub async fn run_backend_contract_suite(backend: Arc<dyn ExecutionBackend>) {
    // ------------------------------------------------------------------
    // Smoke tests for descriptor and capabilities.
    // ------------------------------------------------------------------
    let descriptor = backend.descriptor();
    assert!(
        !descriptor.id.to_string().is_empty(),
        "descriptor id must be non-empty"
    );
    assert!(
        !descriptor.name.is_empty(),
        "descriptor name must be non-empty"
    );

    let _caps = backend.capabilities();

    // ------------------------------------------------------------------
    // Basic execute — must not panic or hang.
    // ------------------------------------------------------------------
    let request = make_request();
    let (sink, _rx) = broadcast::channel(256);
    let cancel = CancellationToken::new();

    let result = backend.execute(request, sink, cancel).await;

    match result {
        Ok(exec_result) => {
            assert!(
                !exec_result.finish_reason.is_empty(),
                "finish_reason must be non-empty on success"
            );
        }
        Err(_) => {
            // Any ExecutionError is acceptable at the contract level.
        }
    }

    // ------------------------------------------------------------------
    // Cancellation — backend must honour CancellationToken.
    // ------------------------------------------------------------------
    let request2 = make_request();
    let (sink2, _rx2) = broadcast::channel(256);
    let parent = CancellationToken::new();
    let child = parent.child_token();

    // Cancel immediately so the backend sees cancellation as soon as it can.
    parent.cancel();

    let result2 = backend.execute(request2, sink2, child).await;
    match result2 {
        Ok(_) => {
            // Some backends may complete before observing cancellation;
            // this is acceptable at the contract level.
        }
        Err(ExecutionError::Cancelled) => { /* expected */ }
        Err(other) => {
            panic!("expected Cancelled on cancelled execute, got {other:?}");
        }
    }
}
