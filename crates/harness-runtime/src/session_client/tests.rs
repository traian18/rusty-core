use std::sync::Arc;
use std::time::Duration;

use harness_protocol::backend::{ExecutionError, ExecutionEvent, ExecutionResult};
use harness_protocol::commands::UserInput;
use harness_protocol::ids::RequestId;
use harness_protocol::tools::AgentToolset;
use harness_protocol::usage::{Cost, ModelUsage, UsageValue};

use crate::scheduler::{Scheduler, SchedulerConfig};
use crate::testing::{FakeBackend, FakeToolRegistry};
use crate::traits::EventSink;
use crate::workspace::FakeWorkspace;

use super::*;

struct NoopSink;
impl EventSink for NoopSink {
    fn send(&self, _envelope: AgentEventEnvelope) {}
}

/// Task 2.8 acceptance criterion: `session.snapshot()` immediately after
/// `session.send(Prompt)` (before completion) shows an in-flight status,
/// and after completion shows `Completed` with populated usage.
#[tokio::test]
async fn snapshot_reflects_in_flight_then_completed_status() {
    let session_id = SessionId::new();
    let request_id = RequestId::new();
    let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));

    let backend = Arc::new(
        FakeBackend::new()
            .with_events(vec![ExecutionEvent::TextDelta {
                request_id,
                delta: "hi".into(),
            }])
            .with_result(ExecutionResult {
                request_id,
                usage: ModelUsage {
                    input_tokens: UsageValue::new(Some(3)),
                    output_tokens: UsageValue::new(Some(4)),
                    total_tokens: UsageValue::new(Some(7)),
                    ..Default::default()
                },
                cost: Cost::default(),
                finish_reason: "end_turn".into(),
            }),
    );
    let tool_registry = Arc::new(FakeToolRegistry::new());
    let workspace = Arc::new(FakeWorkspace::new());

    let runtime = Arc::new(SessionRuntime::new_with_scheduler(
        session_id,
        backend,
        tool_registry,
        workspace,
        Arc::new(NoopSink),
        AgentToolset {
            tools: std::collections::HashMap::new(),
        },
        scheduler,
        None,
    ));
    let client = SessionClient::new(runtime);

    let mut subscriber = client.subscribe();

    client
        .send(SessionCommand::Prompt(UserInput {
            text: "hello".into(),
            attachments: vec![],
        }))
        .await
        .expect("send should succeed");

    // Immediately after sending, before completion, the snapshot should
    // reflect an in-flight status rather than the pre-send default.
    let mut saw_running = false;
    for _ in 0..25 {
        let snap = client.snapshot();
        if snap.status == SessionStatus::Running {
            saw_running = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(
        saw_running,
        "snapshot should show Running while the run is in flight"
    );

    // Drain until Completed is observed on the event stream.
    let mut completed = false;
    for _ in 0..50 {
        while let Ok(envelope) = subscriber.try_recv() {
            if matches!(
                envelope.event,
                harness_protocol::events::AgentEvent::Completed { .. }
            ) {
                completed = true;
            }
        }
        if completed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(completed, "run should complete within the polling window");

    // Give the runner one more tick to publish post-completion status.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let after = client.snapshot();
    assert_eq!(after.status, SessionStatus::Completed);
    assert_eq!(
        after.root_agent_status.metrics.total_tokens.value(),
        Some(7),
        "usage should be populated from the scripted ExecutionResult after completion"
    );
    assert_eq!(after.usage.cumulative.total_requests, 1);
}

/// RST-004: a backend/agent failure must never be reported through
/// `snapshot().status` as a successful completion. `AgentStatus::Failed`
/// (in-flight) and `AgentOutcome::Failed` (post-hoc, once the agent has
/// returned to `Idle`) must both project to `SessionStatus::Failed`,
/// distinct from `SessionStatus::Completed`.
#[tokio::test]
async fn snapshot_reports_failed_status_truthfully() {
    let session_id = SessionId::new();
    let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));

    let backend = Arc::new(FakeBackend::new().with_error(ExecutionError::BackendError {
        message: "scripted failure".into(),
        code: "TEST_FAILURE".into(),
    }));
    let tool_registry = Arc::new(FakeToolRegistry::new());
    let workspace = Arc::new(FakeWorkspace::new());

    let runtime = Arc::new(SessionRuntime::new_with_scheduler(
        session_id,
        backend,
        tool_registry,
        workspace,
        Arc::new(NoopSink),
        AgentToolset {
            tools: std::collections::HashMap::new(),
        },
        scheduler,
        None,
    ));
    let client = SessionClient::new(runtime);

    let mut subscriber = client.subscribe();

    client
        .send(SessionCommand::Prompt(UserInput {
            text: "hello".into(),
            attachments: vec![],
        }))
        .await
        .expect("send should succeed");

    let mut failed_event_seen = false;
    for _ in 0..50 {
        while let Ok(envelope) = subscriber.try_recv() {
            if matches!(
                envelope.event,
                harness_protocol::events::AgentEvent::Failed { .. }
            ) {
                failed_event_seen = true;
            }
        }
        if failed_event_seen {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        failed_event_seen,
        "run should fail within the polling window"
    );

    tokio::time::sleep(Duration::from_millis(20)).await;

    let after = client.snapshot();
    assert_eq!(
        after.status,
        SessionStatus::Failed,
        "a failed run must never be reported as SessionStatus::Completed"
    );
}
