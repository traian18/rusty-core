//! Integration test: full e2e session with scripted backend verifying the exact
//! ordered [`AgentEvent`] sequence emitted for a prompt that streams assistant text.
//!
//! This is the Phase 2 goal acceptance test (spec §71 Phase 2 goal verbatim):
//!
//! > `session.send(prompt)` / `session.subscribe()` works end-to-end against a
//! > fake backend

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_engine::Harness;
use harness_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
    ExecutionResult,
};
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, AgentOutcome};
use harness_protocol::ids::RequestId;
use harness_protocol::tools::ToolDescriptor;
use harness_protocol::usage::{Cost, ModelUsage};
use harness_runtime::traits::{ExecutionBackend, ToolExecutor, ToolRegistry};

// ---------------------------------------------------------------------------
// ScriptedBackend
// ---------------------------------------------------------------------------

/// A deterministic [`ExecutionBackend`] that replays a fixed script of
/// [`ExecutionEvent`]s and returns a pre-set [`ExecutionResult`].
struct ScriptedBackend {
    descriptor: BackendDescriptor,
    script: Vec<ExecutionEvent>,
    finish: ExecutionResult,
}

impl ScriptedBackend {
    fn new(script: Vec<ExecutionEvent>, finish: ExecutionResult) -> Self {
        Self {
            descriptor: BackendDescriptor {
                id: harness_protocol::ids::BackendId::new(),
                name: "scripted".into(),
                description: "Scripted test backend".into(),
                capabilities: BackendCapabilities {
                    streaming: true,
                    ..Default::default()
                },
            },
            script,
            finish,
        }
    }
}

#[async_trait]
impl ExecutionBackend for ScriptedBackend {
    fn descriptor(&self) -> BackendDescriptor {
        self.descriptor.clone()
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.descriptor.capabilities.clone()
    }

    async fn execute(
        &self,
        _request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        _cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        for event in &self.script {
            let _ = sink.send(event.clone());
        }
        Ok(self.finish.clone())
    }
}

// ---------------------------------------------------------------------------
// NoTools — empty ToolRegistry
// ---------------------------------------------------------------------------

struct NoTools;

impl ToolRegistry for NoTools {
    fn lookup(&self, _name: &str) -> Option<Arc<dyn ToolExecutor>> {
        None
    }
    fn descriptors(&self) -> Vec<ToolDescriptor> {
        vec![]
    }
}

// ---------------------------------------------------------------------------
// Helper: drain events from a broadcast receiver non-blockingly
// ---------------------------------------------------------------------------

fn drain_events(rx: &mut broadcast::Receiver<AgentEventEnvelope>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(envelope) = rx.try_recv() {
        events.push(envelope.event);
    }
    events
}

// ---------------------------------------------------------------------------
// session_send_subscribe_e2e
// ---------------------------------------------------------------------------

/// Phase 2 acceptance test: script a backend to emit two `TextDelta` events
/// followed by `Completed`, start a session, send a prompt, subscribe, and
/// verify the exact ordered `AgentEvent` sequence.
#[tokio::test]
async fn session_send_subscribe_e2e() {
    // ── Arrange: scripted backend ─────────────────────────────────────────
    let request_id = RequestId::new();

    let script = vec![
        ExecutionEvent::TextDelta {
            request_id,
            delta: "Hello ".into(),
        },
        ExecutionEvent::TextDelta {
            request_id,
            delta: "world!".into(),
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
    ];

    let finish = ExecutionResult {
        request_id,
        usage: ModelUsage::default(),
        cost: Cost::default(),
        finish_reason: "end_turn".into(),
    };

    let backend = Arc::new(ScriptedBackend::new(script, finish));
    let tool_registry = Arc::new(NoTools);

    // ── Act: builder chain ────────────────────────────────────────────────
    let handle = Harness::new()
        .session()
        .backend(backend)
        .tools(tool_registry)
        .start()
        .await
        .expect("SessionBuilder::start() should succeed");

    // Subscribe before sending the prompt so we don't miss events.
    let mut rx = handle.subscribe();

    // Send a prompt – this kicks off StartRun → backend execution.
    handle
        .send("hello from test")
        .await
        .expect("SessionHandle::send() should succeed");

    // ── Collect events ────────────────────────────────────────────────────
    let mut all_events: Vec<AgentEvent> = Vec::new();

    for _ in 0..50 {
        let batch = drain_events(&mut rx);
        if !batch.is_empty() {
            all_events.extend(batch);
            if all_events
                .iter()
                .any(|e| matches!(e, AgentEvent::Completed { .. }))
            {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // ── Assert the exact ordered event sequence ───────────────────────────
    //
    // The Phase 1 state machine produces these events for a
    // TextDelta("Hello ") → TextDelta("world!") → Completed sequence:
    //   StateChanged(Idle → PreparingContext)
    //   RunStarted
    //   StateChanged(PreparingContext → Streaming)
    //   AssistantTextDelta("Hello ")
    //   AssistantTextDelta("world!")
    //   StateChanged(Streaming → Idle)
    //   Completed { outcome: Success }
    //
    // After filtering out StateChanged:
    //   0. RunStarted
    //   1. AssistantTextDelta("Hello ")
    //   2. AssistantTextDelta("world!")
    //   3. Completed

    // Filter out StateChanged events to see the core sequence.
    let core_events: Vec<&AgentEvent> = all_events
        .iter()
        .filter(|e| !matches!(e, AgentEvent::StateChanged { .. }))
        .collect();

    assert!(
        core_events.len() >= 4,
        "expected at least 4 non-StateChanged events (RunStarted, 2x AssistantTextDelta, Completed); got {}: {:?}",
        core_events.len(),
        core_events
    );

    // 0. RunStarted
    assert!(
        matches!(core_events[0], AgentEvent::RunStarted { .. }),
        "event[0] should be RunStarted, got {:?}",
        core_events[0]
    );

    // 1. AssistantTextDelta("Hello ")
    assert!(
        matches!(&core_events[1], AgentEvent::AssistantTextDelta { delta, .. } if delta == "Hello "),
        "event[1] should be AssistantTextDelta(\"Hello \"), got {:?}",
        core_events[1]
    );

    // 2. AssistantTextDelta("world!")
    assert!(
        matches!(&core_events[2], AgentEvent::AssistantTextDelta { delta, .. } if delta == "world!"),
        "event[2] should be AssistantTextDelta(\"world!\"), got {:?}",
        core_events[2]
    );

    // 3. Completed
    assert!(
        matches!(core_events[3], AgentEvent::Completed { outcome } if *outcome == AgentOutcome::Success),
        "event[3] should be Completed(Success), got {:?}",
        core_events[3]
    );

    // Verify ordering.
    let run_started_idx = all_events
        .iter()
        .position(|e| matches!(e, AgentEvent::RunStarted { .. }));
    let delta1_idx = all_events
        .iter()
        .position(|e| matches!(e, AgentEvent::AssistantTextDelta { delta, .. } if delta == "Hello "));
    let delta2_idx = all_events
        .iter()
        .position(|e| matches!(e, AgentEvent::AssistantTextDelta { delta, .. } if delta == "world!"));
    let completed_idx = all_events
        .iter()
        .position(|e| matches!(e, AgentEvent::Completed { .. }));

    assert!(
        run_started_idx < delta1_idx,
        "RunStarted should precede first AssistantTextDelta"
    );
    assert!(
        delta1_idx < delta2_idx,
        "first AssistantTextDelta should precede second"
    );
    assert!(
        delta2_idx < completed_idx,
        "second AssistantTextDelta should precede Completed"
    );
}
