//! Session client — external API for interacting with a session.
//!
//! This module provides the frontend-facing `SessionClient` handle for a
//! single session. It wraps the session runtime and translates user-facing
//! `SessionCommand`s into agent-level commands, while offering lightweight
//! read projections (snapshots) and an ordered event subscription view.

use std::sync::Arc;

use tokio::sync::broadcast;

use harness_protocol::commands::AgentStatus;
use harness_protocol::events::{AgentEventEnvelope, AgentOutcome};
use harness_protocol::ids::{AgentId, SessionId, Timestamp};
use harness_protocol::usage::{
    AgentUsageMetrics, AgentUsageSnapshot, CumulativeUsage, SessionUsageSnapshot,
};

use crate::session_runtime::{
    AgentLiveState, SessionCommand, SessionError, SessionRuntime, SessionStatus,
};

/// A lightweight read projection of the session's current state.
///
/// This is a pure function of live state — no stored copy — giving callers
/// the current snapshot of the session status, root agent status, and usage.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    /// The session's unique identifier.
    pub session_id: SessionId,
    /// High-level session status.
    pub status: SessionStatus,
    /// The root agent's identifier (convenience field).
    pub root_agent_id: AgentId,
    /// The root agent's detailed usage/status projection.
    ///
    /// Contains information about the root agent's current run,
    /// its operation, backend, and usage tracking.
    pub root_agent_status: AgentUsageSnapshot,
    /// Aggregated usage information across the session.
    pub usage: SessionUsageSnapshot,
    /// When this snapshot was taken.
    pub timestamp: Timestamp,
}

/// Maps an agent's live runtime status onto the coarser session-level status.
///
/// `AgentStatus` returns to `Idle` after a successful run completes, so a
/// recorded [`AgentOutcome`] (from [`AgentLiveState::last_outcome`]) is what
/// distinguishes "never run" from "completed"/"cancelled" while idle.
fn session_status_from_live(live: &AgentLiveState, fallback: SessionStatus) -> SessionStatus {
    use AgentStatus::*;
    match live.status {
        PreparingContext | WaitingForBackend | Streaming | Executing | WaitingForPermission
        | WaitingForChildren | Paused => SessionStatus::Running,
        Cancelled => SessionStatus::Cancelled,
        Failed | Completed => SessionStatus::Completed,
        Idle => match live.last_outcome {
            Some(AgentOutcome::Success) | Some(AgentOutcome::Failed) => SessionStatus::Completed,
            Some(AgentOutcome::Cancelled) => SessionStatus::Cancelled,
            None => fallback,
        },
    }
}

/// Projects an [`AgentLiveState`] into the coarser [`AgentUsageMetrics`]
/// shape used by the public snapshot protocol type.
fn metrics_from_live(live: &AgentLiveState) -> AgentUsageMetrics {
    AgentUsageMetrics {
        total_runs: if live.last_outcome.is_some() { 1 } else { 0 },
        total_requests: live.total_requests,
        total_tool_calls: 0,
        total_tokens: live.usage.inclusive_usage.total_tokens,
        total_cost: live.total_cost_usd,
    }
}

/// The frontend-facing handle for a live session.
///
/// Owns an `Arc<SessionRuntime>` and provides translations between
/// user-facing `SessionCommand`s and the runtime's internal commands.
/// It also offers a stable ordered stream of events via [`Self::subscribe`].
pub struct SessionClient {
    /// The runtime that owns the session's agents and event bus.
    runtime: Arc<SessionRuntime>,
}

impl SessionClient {
    /// Create a new session client for the given runtime.
    pub fn new(runtime: Arc<SessionRuntime>) -> Self {
        Self { runtime }
    }

    /// Send a user-facing command to the session.
    ///
    /// The runtime performs the command translation and routes the resulting
    /// agent-level command through its mailbox.
    pub async fn send(&self, command: SessionCommand) -> Result<(), SessionError> {
        self.runtime.send_command(command).await
    }

    /// Take a lightweight snapshot of the session's current live state.
    ///
    /// No stored snapshot is kept; the projection is generated from the
    /// runtime's current state — including the live per-agent status/usage
    /// table that every `AgentRunner` publishes to after each transition —
    /// on every call. Immediately after `send(Prompt)` this reflects an
    /// in-flight status (e.g. `Running`); once the run completes it reflects
    /// `Completed` with populated usage.
    pub fn snapshot(&self) -> SessionSnapshot {
        let runtime_snapshot = self.runtime.state_snapshot();
        let live = self.runtime.agent_live_state(runtime_snapshot.root_agent_id);
        let status = session_status_from_live(&live, runtime_snapshot.status);
        let metrics = metrics_from_live(&live);
        let now = Timestamp::now();

        let root_agent_status = AgentUsageSnapshot {
            agent_id: runtime_snapshot.root_agent_id.to_string(),
            metrics: metrics.clone(),
            timestamp: now.to_string(),
        };

        let usage = SessionUsageSnapshot {
            session_id: runtime_snapshot.session_id.to_string(),
            cumulative: CumulativeUsage {
                total_tokens: metrics.total_tokens,
                total_cost: metrics.total_cost,
                total_requests: metrics.total_requests,
            },
            timestamp: now.to_string(),
        };

        SessionSnapshot {
            session_id: runtime_snapshot.session_id,
            status,
            root_agent_id: runtime_snapshot.root_agent_id,
            root_agent_status,
            usage,
            timestamp: now,
        }
    }

    /// Return a fresh subscriber receiver for the session's ordered events.
    ///
    /// Multiple callers can subscribe independently to the same event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEventEnvelope> {
        self.runtime.event_bus.subscribe()
    }

    /// Consume this client and return its underlying runtime.
    pub fn into_runtime(self) -> Arc<SessionRuntime> {
        self.runtime
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use harness_protocol::backend::{ExecutionEvent, ExecutionResult};
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
        assert!(saw_running, "snapshot should show Running while the run is in flight");

        // Drain until Completed is observed on the event stream.
        let mut completed = false;
        for _ in 0..50 {
            while let Ok(envelope) = subscriber.try_recv() {
                if matches!(envelope.event, harness_protocol::events::AgentEvent::Completed { .. }) {
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
}
