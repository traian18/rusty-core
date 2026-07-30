//! A scripted [`ExecutionBackend`] test double.
//!
//! [`FakeBackend`] replays a pre-recorded sequence of [`ExecutionEvent`]s
//! through the sink, honoring cancellation between each event, and then
//! resolves with either a scripted [`ExecutionResult`] or a scripted
//! [`ExecutionError`]. It can also be configured to block indefinitely
//! (simulating an in-flight backend call) until its [`CancellationToken`]
//! fires, which is used to exercise mid-flight cancellation.

use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
    ExecutionResult,
};
use harness_protocol::ids::BackendId;

use crate::traits::ExecutionBackend;

/// A small, realistic per-step delay used to simulate streaming latency.
///
/// Real backends never resolve a multi-event execution synchronously — each
/// streamed delta and the final completion arrive after genuine I/O
/// latency. Without *some* await point between steps, a scripted run can
/// race straight from `StartRun` to `Completed` inside a single scheduler
/// tick, faster than any external observer (e.g. a snapshot-polling test)
/// could ever witness an in-flight status. This constant keeps that window
/// small in absolute terms (tests still complete in well under a second)
/// while guaranteeing it is genuinely observable.
const STEP_DELAY: Duration = Duration::from_millis(2);

/// A scripted execution backend used to exercise runtime orchestration logic
/// without depending on a real provider.
#[derive(Debug, Clone, Default)]
pub struct FakeBackend {
    /// The events to stream through the sink, in order.
    pub scripted_events: Vec<ExecutionEvent>,
    /// The final result to return, if execution should succeed.
    pub scripted_result: Option<ExecutionResult>,
    /// The final error to return, if execution should fail.
    pub scripted_error: Option<ExecutionError>,
    /// If `true`, after streaming `scripted_events` the backend blocks
    /// (awaiting cancellation) instead of returning immediately, simulating
    /// an in-flight call. Used to test mid-flight cancellation.
    pub block_until_cancelled: bool,
}

impl FakeBackend {
    /// Creates a new fake backend with no scripted behavior.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder method to set the scripted events.
    pub fn with_events(mut self, events: Vec<ExecutionEvent>) -> Self {
        self.scripted_events = events;
        self
    }

    /// Builder method to set the scripted successful result.
    pub fn with_result(mut self, result: ExecutionResult) -> Self {
        self.scripted_result = Some(result);
        self
    }

    /// Builder method to set the scripted error.
    pub fn with_error(mut self, error: ExecutionError) -> Self {
        self.scripted_error = Some(error);
        self
    }

    /// Builder method: after streaming `scripted_events`, block until the
    /// cancellation token fires instead of returning a scripted result.
    ///
    /// Used to simulate a backend call that is still in flight so that
    /// mid-flight cancellation can be exercised deterministically.
    pub fn blocking_until_cancelled(mut self) -> Self {
        self.block_until_cancelled = true;
        self
    }
}

#[async_trait]
impl ExecutionBackend for FakeBackend {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: BackendId::new(),
            name: "fake-backend".to_string(),
            description: "A scripted backend used for testing.".to_string(),
            capabilities: self.capabilities(),
        }
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            streaming: true,
            reasoning_stream: true,
            tool_calls: true,
            parallel_tool_calls: true,
            host_managed_tools: true,
            backend_managed_tools: false,
            permissions: false,
            images: false,
            resumable_sessions: false,
            native_subagents: false,
            model_switching: false,
            exact_usage: true,
            exact_cost: true,
        }
    }

    async fn execute(
        &self,
        _request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        for event in &self.scripted_events {
            if cancel.is_cancelled() {
                return Err(ExecutionError::Cancelled);
            }

            // Ignore send errors: a scripted test may not have any
            // subscribers listening on the sink.
            let _ = sink.send(event.clone());

            if cancel.is_cancelled() {
                return Err(ExecutionError::Cancelled);
            }

            // Simulate genuine streaming latency between deltas. Without
            // this, a scripted multi-event run can resolve entirely within
            // a single scheduler tick, making the in-flight state
            // unobservable to any external poller (see `STEP_DELAY`'s doc
            // comment).
            tokio::select! {
                _ = tokio::time::sleep(STEP_DELAY) => {}
                _ = cancel.cancelled() => return Err(ExecutionError::Cancelled),
            }
        }

        if self.block_until_cancelled {
            cancel.cancelled().await;
            return Err(ExecutionError::Cancelled);
        }

        // Simulate the latency of the final completion/error response
        // itself, even when there were no intermediate streamed events, so
        // the in-flight status remains observable for at least one tick.
        tokio::select! {
            _ = tokio::time::sleep(STEP_DELAY) => {}
            _ = cancel.cancelled() => return Err(ExecutionError::Cancelled),
        }

        if let Some(error) = self.scripted_error.clone() {
            return Err(error);
        }

        if let Some(result) = self.scripted_result.clone() {
            return Ok(result);
        }

        Err(ExecutionError::BackendError {
            message: "FakeBackend has no scripted result or error".to_string(),
            code: "FAKE_BACKEND_UNSCRIPTED".to_string(),
        })
    }
}
