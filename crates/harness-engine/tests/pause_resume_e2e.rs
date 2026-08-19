//! End-to-end coverage for `SessionHandle::pause` / `SessionHandle::resume`.
//!
//! The deterministic core already implemented pause/resume
//! (`harness_core::transitions::{pause, resume}`, `AgentStatus::Paused`) and
//! `SessionCommand::Pause`/`Resume` already mapped onto `AgentCommand`, but
//! nothing exposed it: there was no method on `SessionHandle`, no
//! `MutationCommand` variant, and `ProtocolCapabilities::pause_resume` was
//! hard-coded `false`. These tests pin the now-reachable path from the
//! public engine API down through the state machine.
//!
//! # What is and isn't covered here
//!
//! Covered: the transitions reachable deterministically through the public
//! API — pausing an idle session, resuming it, and the state machine's
//! no-op guards.
//!
//! Not covered: pausing *mid-request*. `AgentRunner::run` receives one
//! command, then awaits `dispatch_effects` (which awaits the backend) before
//! receiving the next, so a `Pause` sent while a backend request is in
//! flight applies only once that request settles. That is the documented
//! behavior on `SessionHandle::pause`, not something this file asserts,
//! because scheduling it deterministically would require gating the fake
//! backend on a signal and would pin timing rather than semantics.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_engine::Harness;
use harness_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
    ExecutionResult,
};
use harness_protocol::commands::AgentStatus;
use harness_protocol::events::{AgentEvent, AgentEventEnvelope};
use harness_protocol::ids::RequestId;
use harness_protocol::usage::{Cost, ModelUsage};
use harness_runtime::traits::ExecutionBackend;
use harness_tools::registry::ToolRegistry;
use harness_tools::ToolDescriptor;

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// A backend that completes a run immediately, so the session settles back
/// to `Idle` and pause/resume can be exercised against a known state.
struct ImmediateBackend {
    descriptor: BackendDescriptor,
}

impl ImmediateBackend {
    fn new() -> Self {
        Self {
            descriptor: BackendDescriptor {
                id: harness_protocol::ids::BackendId::new(),
                name: "immediate".into(),
                description: "Completes every request without streaming".into(),
                capabilities: BackendCapabilities {
                    streaming: true,
                    ..Default::default()
                },
            },
        }
    }
}

#[async_trait]
impl ExecutionBackend for ImmediateBackend {
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
        let request_id = RequestId::new();
        let result = ExecutionResult {
            request_id,
            usage: ModelUsage::default(),
            cost: Cost::default(),
            finish_reason: "end_turn".into(),
        };
        let _ = sink.send(ExecutionEvent::Completed {
            request_id,
            result: result.clone(),
        });
        Ok(result)
    }
}

struct NoTools;

#[async_trait]
impl ToolRegistry for NoTools {
    fn register(
        &self,
        _executor: Arc<dyn harness_tools::ToolExecutor>,
    ) -> Result<(), harness_tools::registry::RegistrationError> {
        Ok(())
    }

    fn get_executor(&self, _tool_id: &str) -> Option<Arc<dyn harness_tools::ToolExecutor>> {
        None
    }

    fn descriptors(&self) -> Vec<ToolDescriptor> {
        vec![]
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn drain_events(rx: &mut broadcast::Receiver<AgentEventEnvelope>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(envelope) = rx.try_recv() {
        events.push(envelope.event);
    }
    events
}

/// Poll until `predicate` matches a drained event or the budget runs out,
/// accumulating everything seen so failures can report the real sequence.
async fn wait_for(
    rx: &mut broadcast::Receiver<AgentEventEnvelope>,
    seen: &mut Vec<AgentEvent>,
    predicate: impl Fn(&AgentEvent) -> bool,
) -> bool {
    for _ in 0..100 {
        seen.extend(drain_events(rx));
        if seen.iter().any(&predicate) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    false
}

fn state_changes(events: &[AgentEvent]) -> Vec<(AgentStatus, AgentStatus)> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::StateChanged { from, to } => Some((*from, *to)),
            _ => None,
        })
        .collect()
}

async fn idle_session() -> harness_engine::SessionHandle {
    Harness::new()
        .session()
        .backend(Arc::new(ImmediateBackend::new()))
        .tools(Arc::new(NoTools))
        .start()
        .await
        .expect("session should start")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The headline case: an idle session pauses and resumes, and both
/// transitions are observable on the event stream. Before this change there
/// was no way to reach either from the engine's public API.
#[tokio::test]
async fn pause_and_resume_round_trip_through_the_public_api() {
    let handle = idle_session().await;
    let mut rx = handle.subscribe();
    let mut seen = Vec::new();

    handle.pause().await.expect("pause should be accepted");
    assert!(
        wait_for(&mut rx, &mut seen, |event| matches!(
            event,
            AgentEvent::StateChanged {
                to: AgentStatus::Paused,
                ..
            }
        ))
        .await,
        "expected a StateChanged into Paused, saw: {:?}",
        state_changes(&seen)
    );

    handle.resume().await.expect("resume should be accepted");
    assert!(
        wait_for(&mut rx, &mut seen, |event| matches!(
            event,
            AgentEvent::StateChanged {
                from: AgentStatus::Paused,
                to: AgentStatus::Idle,
            }
        ))
        .await,
        "expected a StateChanged from Paused back to Idle, saw: {:?}",
        state_changes(&seen)
    );

    handle.close().await.expect("close should succeed");
}

/// `resume` on a session that was never paused must not manufacture a
/// transition — the state machine returns no effects, so nothing reaches the
/// event stream. This is what makes it safe for a UI to send `resume`
/// without first checking state.
#[tokio::test]
async fn resume_without_a_prior_pause_is_a_no_op() {
    let handle = idle_session().await;
    let mut rx = handle.subscribe();

    handle.resume().await.expect("resume should be accepted");

    let mut seen = Vec::new();
    let observed_transition = wait_for(&mut rx, &mut seen, |event| {
        matches!(event, AgentEvent::StateChanged { .. })
    })
    .await;

    assert!(
        !observed_transition,
        "resume on a non-paused agent should emit nothing, saw: {:?}",
        state_changes(&seen)
    );

    handle.close().await.expect("close should succeed");
}

/// Pausing twice emits one transition, not two: the second is swallowed by
/// the state machine's `Paused | Cancelled | Failed` guard.
#[tokio::test]
async fn pausing_an_already_paused_session_is_a_no_op() {
    let handle = idle_session().await;
    let mut rx = handle.subscribe();
    let mut seen = Vec::new();

    handle
        .pause()
        .await
        .expect("first pause should be accepted");
    assert!(
        wait_for(&mut rx, &mut seen, |event| matches!(
            event,
            AgentEvent::StateChanged {
                to: AgentStatus::Paused,
                ..
            }
        ))
        .await,
        "expected the first pause to transition into Paused"
    );

    handle
        .pause()
        .await
        .expect("second pause should still be accepted by the channel");

    // Give the runner a chance to process the redundant command.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    seen.extend(drain_events(&mut rx));

    let into_paused = state_changes(&seen)
        .into_iter()
        .filter(|(_, to)| *to == AgentStatus::Paused)
        .count();
    assert_eq!(
        into_paused,
        1,
        "a redundant pause must not emit a second transition, saw: {:?}",
        state_changes(&seen)
    );

    handle.close().await.expect("close should succeed");
}
