use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;

use harness_protocol::backend::{ExecutionError, ExecutionEvent, ExecutionResult};
use harness_protocol::commands::UserInput;
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, AgentOutcome};
use harness_protocol::ids::{RequestId, SessionId, Timestamp};
use harness_protocol::tools::AgentToolset;
use harness_protocol::usage::{Cost, ModelUsage};
use harness_session_store::{JsonlSessionStore, SessionStore};

use crate::scheduler::{Scheduler, SchedulerConfig};
use crate::testing::{FakeBackend, FakeToolRegistry};
use crate::traits::EventSink;
use crate::workspace::FakeWorkspace;

use super::*;

// -----------------------------------------------------------------------
// Helper: drain buffered events from a broadcast receiver non-blockingly
// -----------------------------------------------------------------------

fn drain_events(rx: &mut broadcast::Receiver<AgentEventEnvelope>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(envelope) = rx.try_recv() {
        events.push(envelope.event);
    }
    events
}

/// Drains buffered envelopes from a broadcast receiver non-blockingly.
fn drain_envelopes(rx: &mut broadcast::Receiver<AgentEventEnvelope>) -> Vec<AgentEventEnvelope> {
    let mut envelopes = Vec::new();
    while let Ok(envelope) = rx.try_recv() {
        envelopes.push(envelope);
    }
    envelopes
}

// -----------------------------------------------------------------------
// Test: session event bus produces a well-ordered event stream
// -----------------------------------------------------------------------

/// Verifies that subscribing to the session event bus and sending
/// `SessionCommand::Prompt` produces a well-ordered stream.
///
/// The Phase 1 state machine emits these events for a
/// TextDelta → Completed sequence:
///   StateChanged(Idle → PreparingContext)
///   RunStarted
///   StateChanged(PreparingContext → Streaming)
///   AssistantTextDelta("Hello, world!")
///   StateChanged(Streaming → Idle)
///   Completed { Success }
#[tokio::test]
async fn session_prompt_produces_ordered_event_stream() {
    // ── Setup ────────────────────────────────────────
    let session_id = SessionId::new();
    let request_id = RequestId::new();

    let backend = Arc::new(
        FakeBackend::new()
            .with_events(vec![
                ExecutionEvent::TextDelta {
                    request_id,
                    delta: "Hello, world!".into(),
                },
                ExecutionEvent::Completed {
                    request_id,
                    result: ExecutionResult {
                        request_id,
                        usage: ModelUsage::default(),
                        cost: Cost::default(),
                        finish_reason: "end_turn".into(),
                    },
                },
            ])
            .with_result(ExecutionResult {
                request_id,
                usage: ModelUsage::default(),
                cost: Cost::default(),
                finish_reason: "end_turn".into(),
            }),
    );

    let tool_registry = Arc::new(FakeToolRegistry::new());
    let workspace = Arc::new(FakeWorkspace::new());

    // Use a no-op event sink for the external persistence/logging path.
    struct NoopSink;
    impl EventSink for NoopSink {
        fn send(&self, _envelope: AgentEventEnvelope) {}
    }

    let runtime = SessionRuntime::new(
        session_id,
        backend,
        tool_registry,
        workspace,
        Arc::new(NoopSink),
    );

    // Subscribe to the event bus before sending any command.
    let mut subscriber = runtime.event_bus.subscribe();

    // ── Act: send Prompt command ────────────────────────
    runtime
        .send_command(SessionCommand::Prompt(UserInput {
            text: "hello".to_string(),
            attachments: vec![],
        }))
        .await
        .expect("send_command should succeed");

    // ── Collect events ─────────────────────────────
    let mut all_events: Vec<AgentEvent> = Vec::new();
    for _ in 0..30 {
        let batch = drain_events(&mut subscriber);
        if !batch.is_empty() {
            all_events.extend(batch);
            if all_events
                .iter()
                .any(|e| matches!(e, AgentEvent::Completed { .. }))
            {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // ── Assert ──────────────────────────────────
    // Filter out StateChanged events to see the core sequence.
    let core_events: Vec<&AgentEvent> = all_events
        .iter()
        .filter(|e| !matches!(e, AgentEvent::StateChanged { .. }))
        .collect();

    // Expected core events after StateChanged filtering:
    //   0. RunStarted
    //   1. AssistantTextDelta("Hello, world!")
    //   2. Completed { outcome: Success }
    assert!(
        core_events.len() >= 3,
        "expected at least 3 non-StateChanged events (RunStarted, AssistantTextDelta, Completed); got {}: {:?}",
        core_events.len(),
        core_events,
    );

    assert!(
        matches!(core_events[0], AgentEvent::RunStarted { .. }),
        "event[0] should be RunStarted, got {:?}",
        core_events[0]
    );

    assert!(
        matches!(&core_events[1], AgentEvent::AssistantTextDelta { delta, .. } if delta == "Hello, world!"),
        "event[1] should be AssistantTextDelta(\"Hello, world!\"), got {:?}",
        core_events[1]
    );

    assert!(
        matches!(core_events[2], AgentEvent::Completed { outcome } if *outcome == AgentOutcome::Success),
        "event[2] should be Completed(Success), got {:?}",
        core_events[2]
    );

    // Verify ordering.
    let run_started_idx = all_events
        .iter()
        .position(|e| matches!(e, AgentEvent::RunStarted { .. }));
    let delta_idx = all_events
        .iter()
        .position(|e| matches!(e, AgentEvent::AssistantTextDelta { .. }));
    let completed_idx = all_events
        .iter()
        .position(|e| matches!(e, AgentEvent::Completed { .. }));

    assert!(
        run_started_idx < delta_idx,
        "RunStarted should precede AssistantTextDelta"
    );
    assert!(
        delta_idx < completed_idx,
        "AssistantTextDelta should precede Completed"
    );
}

// -----------------------------------------------------------------------
// Test: agent_live_state reflects live status transitions and usage
// -----------------------------------------------------------------------

/// Verifies that [`SessionRuntime::agent_live_state`] is a truthful,
/// continuously-updated projection: it starts `Idle`, transitions away
/// from `Idle` while the run is in flight, and ends with a recorded
/// `Success` outcome and non-empty usage once the run completes.
#[tokio::test]
async fn agent_live_state_reflects_run_lifecycle() {
    let session_id = SessionId::new();
    let request_id = RequestId::new();

    let backend = Arc::new(
        FakeBackend::new()
            .with_events(vec![ExecutionEvent::TextDelta {
                request_id,
                delta: "hi".into(),
            }])
            .with_result(ExecutionResult {
                request_id,
                usage: ModelUsage {
                    input_tokens: harness_protocol::usage::UsageValue::new(Some(5)),
                    output_tokens: harness_protocol::usage::UsageValue::new(Some(7)),
                    total_tokens: harness_protocol::usage::UsageValue::new(Some(12)),
                    ..Default::default()
                },
                cost: Cost::default(),
                finish_reason: "end_turn".into(),
            }),
    );
    let tool_registry = Arc::new(FakeToolRegistry::new());
    let workspace = Arc::new(FakeWorkspace::new());

    struct NoopSink;
    impl EventSink for NoopSink {
        fn send(&self, _envelope: AgentEventEnvelope) {}
    }

    let runtime = SessionRuntime::new(
        session_id,
        backend,
        tool_registry,
        workspace,
        Arc::new(NoopSink),
    );

    let root_id = runtime
        .state
        .lock()
        .expect("state mutex poisoned")
        .root_agent_id;

    // Before any command, live state is the default (Idle, no outcome).
    let before = runtime.agent_live_state(root_id);
    assert_eq!(before.status, AgentStatus::Idle);
    assert!(before.last_outcome.is_none());

    let mut subscriber = runtime.event_bus.subscribe();

    runtime
        .send_command(SessionCommand::Prompt(UserInput {
            text: "hello".to_string(),
            attachments: vec![],
        }))
        .await
        .expect("send_command should succeed");

    // Poll until a Completed event has been observed.
    let mut completed = false;
    for _ in 0..50 {
        let batch = drain_events(&mut subscriber);
        if batch
            .iter()
            .any(|e| matches!(e, AgentEvent::Completed { .. }))
        {
            completed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(completed, "run should complete within the polling window");

    // Give the runner one more tick to publish the post-completion status.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let after = runtime.agent_live_state(root_id);
    assert_eq!(
        after.last_outcome,
        Some(AgentOutcome::Success),
        "last_outcome should be Success after the run completes"
    );
    assert_eq!(
        after.usage.inclusive_usage.total_tokens.value(),
        Some(12),
        "usage should be populated from the scripted ExecutionResult"
    );
    assert_eq!(after.total_requests, 1);
}

#[tokio::test]
async fn session_root_remains_available_across_completed_runs() {
    let session_id = SessionId::new();
    let request_id = RequestId::new();
    let backend = Arc::new(FakeBackend::new().with_result(ExecutionResult {
        request_id,
        usage: ModelUsage::default(),
        cost: Cost::default(),
        finish_reason: "end_turn".into(),
    }));

    struct NoopSink;
    impl EventSink for NoopSink {
        fn send(&self, _envelope: AgentEventEnvelope) {}
    }

    let runtime = SessionRuntime::new(
        session_id,
        backend,
        Arc::new(FakeToolRegistry::new()),
        Arc::new(FakeWorkspace::new()),
        Arc::new(NoopSink),
    );
    let root_id = runtime.state_snapshot().root_agent_id;

    for prompt in ["first", "second"] {
        runtime
            .send_command(SessionCommand::Prompt(UserInput {
                text: prompt.into(),
                attachments: vec![],
            }))
            .await
            .expect("root mailbox remains available");
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let live = runtime.agent_live_state(root_id);
                let expected_requests = if prompt == "first" { 1 } else { 2 };
                if live.status == AgentStatus::Idle && live.total_requests >= expected_requests {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("run should complete");
        assert_eq!(runtime.state_snapshot().status, SessionStatus::Completed);
    }
}

#[tokio::test]
async fn partial_provider_stream_failure_has_a_truthful_terminal_state() {
    let session_id = SessionId::new();
    let request_id = RequestId::new();
    let backend = Arc::new(
        FakeBackend::new()
            .with_events(vec![ExecutionEvent::TextDelta {
                request_id,
                delta: "partial".into(),
            }])
            .with_error(ExecutionError::BackendError {
                message: "stream disconnected".into(),
                code: "SCRIPTED_DISCONNECT".into(),
            }),
    );

    struct NoopSink;
    impl EventSink for NoopSink {
        fn send(&self, _envelope: AgentEventEnvelope) {}
    }

    let runtime = SessionRuntime::new(
        session_id,
        backend,
        Arc::new(FakeToolRegistry::new()),
        Arc::new(FakeWorkspace::new()),
        Arc::new(NoopSink),
    );
    let root_id = runtime.state_snapshot().root_agent_id;
    let mut subscriber = runtime.event_bus.subscribe();
    runtime
        .send_command(SessionCommand::Prompt(UserInput {
            text: "stream".into(),
            attachments: vec![],
        }))
        .await
        .expect("start run");

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if runtime.agent_live_state(root_id).status == AgentStatus::Failed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider failure should terminate the run");

    tokio::time::sleep(Duration::from_millis(20)).await;
    let events = drain_events(&mut subscriber);
    let partial_index = events
        .iter()
        .position(|event| matches!(event, AgentEvent::AssistantTextDelta { delta, .. } if delta == "partial"))
        .expect("partial delta is published");
    let failed_index = events
        .iter()
        .position(|event| matches!(event, AgentEvent::Failed { .. }))
        .expect("failure event is published");
    assert!(partial_index < failed_index);
    let snapshot = runtime.state_snapshot();
    assert_eq!(snapshot.status, SessionStatus::Failed);
    assert!(snapshot.error.is_some());
}

// -----------------------------------------------------------------------
// Test: RC-301 — stored and observed order agree through the committer
// -----------------------------------------------------------------------

/// With a durable store configured, events flow through the session's
/// authoritative committer: subscribers observe the exact sequences the
/// store persisted, ephemeral deltas are not persisted, and a terminal
/// run triggers a checkpoint (RC-302).
#[tokio::test]
async fn committed_events_match_stored_order() {
    let dir = std::env::temp_dir().join(format!(
        "harness-runtime-rc301-{}-{}",
        std::process::id(),
        Timestamp::now().timestamp_millis()
    ));
    let store: Arc<dyn SessionStore> = Arc::new(JsonlSessionStore::new(&dir));
    let session_id = SessionId::new();
    let request_id = RequestId::new();

    let backend = Arc::new(
        FakeBackend::new()
            .with_events(vec![
                ExecutionEvent::TextDelta {
                    request_id,
                    delta: "hello".into(),
                },
                ExecutionEvent::Completed {
                    request_id,
                    result: ExecutionResult {
                        request_id,
                        usage: ModelUsage::default(),
                        cost: Cost::default(),
                        finish_reason: "end_turn".into(),
                    },
                },
            ])
            .with_result(ExecutionResult {
                request_id,
                usage: ModelUsage::default(),
                cost: Cost::default(),
                finish_reason: "end_turn".into(),
            }),
    );
    let tool_registry = Arc::new(FakeToolRegistry::new());
    let workspace = Arc::new(FakeWorkspace::new());

    struct NoopSink;
    impl EventSink for NoopSink {
        fn send(&self, _envelope: AgentEventEnvelope) {}
    }

    let runtime = SessionRuntime::new_with_scheduler(
        session_id,
        backend,
        tool_registry,
        workspace,
        Arc::new(NoopSink),
        AgentToolset {
            tools: HashMap::new(),
        },
        Arc::new(Scheduler::new(SchedulerConfig::default())),
        Some(store.clone()),
    );

    let mut subscriber = runtime.event_bus.subscribe();
    runtime
        .send_command(SessionCommand::Prompt(UserInput {
            text: "hello".to_string(),
            attachments: vec![],
        }))
        .await
        .expect("send_command should succeed");

    // Collect observed sequences (committer-assigned) until the run
    // completes.
    let mut observed_sequences: Vec<u64> = Vec::new();
    let mut completed = false;
    for _ in 0..50 {
        let batch = drain_envelopes(&mut subscriber);
        for envelope in batch {
            if let Some(sequence) = envelope.session_sequence {
                observed_sequences.push(sequence);
            }
            if matches!(envelope.event, AgentEvent::Completed { .. }) {
                completed = true;
            }
        }
        if completed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(completed, "run should complete");

    // Wait for the terminal checkpoint to land in the store.
    let mut snapshot_seen = false;
    for _ in 0..50 {
        if let Ok(stored) = store.load_session(session_id).await {
            snapshot_seen = stored.snapshot.is_some();
            if snapshot_seen {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        snapshot_seen,
        "a terminal run must produce an automatic checkpoint"
    );

    // Every durable event the store persisted must carry a final sequence,
    // and the observed stream is strictly increasing overall (stored and
    // observed order agree on the durable events).
    let stored_events = store
        .events_since(session_id, 0)
        .await
        .expect("load committed event history");
    let stored_sequences: Vec<u64> = stored_events
        .iter()
        .filter_map(|event| event.session_sequence)
        .collect();
    assert!(
        !stored_sequences.is_empty(),
        "durable events were persisted with final sequences"
    );
    for pair in observed_sequences.windows(2) {
        assert!(
            pair[1] > pair[0],
            "observed sequences are strictly increasing"
        );
    }
    for stored_sequence in &stored_sequences {
        assert!(
            observed_sequences.contains(stored_sequence),
            "stored sequence {stored_sequence} was observed with the same value"
        );
    }

    // No streaming delta was persisted.
    assert!(
        stored_events
            .iter()
            .all(|event| !matches!(event.envelope.event, AgentEvent::AssistantTextDelta { .. })),
        "ephemeral deltas are never stored"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
