//! Session runtime — event bus, session state, and lifecycle.
//!
//! This module provides [`SessionEventBus`] — the per-session event aggregator
//! that merges event streams from all agent runners into a single ordered stream
//! for subscribers — and [`SessionRuntime`], the top-level orchestrator that
//! owns a session's root agent, cancellation scope, backend bindings, workspace
//! binding, and event bus.
//!
//! # RC-300 additions
//!
//! - The event bus **preserves** a `session_sequence` already assigned by the
//!   session's authoritative [`SessionCommitter`] (RC-301); it only assigns
//!   sequences when no committer is configured, so stored and observed order
//!   always agree when persistence is enabled.
//! - [`SessionRuntime`] retains the durable store and owns the session's
//!   shared committer and per-agent **projection table**
//!   ([`AgentProjectionTable`]) — every runner publishes its
//!   [`StoredAgentState`] after each transition, and [`SessionRuntime::checkpoint`]
//!   (plus the automatic snapshot hooks) build versioned,
//!   dependency-recorded [`DurableSessionSnapshot`]s from those projections.
//!
//! # Architecture & SOLID Principles
//!
//! Each concern below lives in its own file (Single Responsibility
//! Principle) instead of one struct owning all of it:
//!
//! - **Value types ([`types`])**: [`SessionStatus`], [`SessionCommand`],
//!   [`SessionError`], [`SessionSnapshot`], [`SessionState`] — plain data,
//!   no behavior.
//! - **Live status ([`live_state`])**: [`AgentLiveState`] and
//!   [`LiveStateTable`], the per-agent projection every runner keeps fresh.
//! - **Durable projection ([`projection`])**: [`AgentProjectionTable`] and
//!   both directions of the live-agent ⇄ [`StoredAgentState`] conversion
//!   ([`stored_agent_state`], `build_snapshot`, and the restore-side
//!   `capabilities_from_value`/`usage_from_value`).
//! - **Event aggregation ([`event_bus`])**: [`SessionEventBus`] and the
//!   `BridgeEventSink` that forwards to an external sink.
//! - **Automatic checkpoints ([`checkpoint`])**: `RuntimeCheckpointRequester`,
//!   the [`CheckpointRequester`](harness_session_store::CheckpointRequester)
//!   the committer calls into.
//! - **Runner spawning ([`runner_spawn`])**: the one place that opens an
//!   agent's task channel, wires it into the supervisor/live-state/
//!   projection/event-bus, and spawns it — shared by fresh construction,
//!   restore, and dynamically spawned agents so those three lifecycles
//!   cannot drift out of sync.
//! - **Fresh construction ([`construction`])**: `SessionRuntime::new*`.
//! - **Restore construction ([`restoration`])**: `SessionRuntime::from_stored`.
//! - **Runtime ([`SessionRuntime`], this file)**: the struct definition and
//!   its steady-state operations (mailbox commands, cancellation, snapshots,
//!   checkpoints) — everything a session does once it is already running.

mod checkpoint;
mod construction;
mod event_bus;
mod live_state;
mod projection;
mod restoration;
mod runner_spawn;
mod types;

pub use event_bus::SessionEventBus;
pub use live_state::{AgentLiveState, LiveStateTable};
pub use projection::AgentProjectionTable;
pub use types::{SessionCommand, SessionError, SessionSnapshot, SessionState, SessionStatus};

pub(crate) use projection::stored_agent_state;

#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use harness_core::agent::Agent;
use harness_protocol::commands::{AgentCommand, AgentStatus, PermissionDecision, UserInput};
use harness_protocol::events::AgentOutcome;
use harness_protocol::ids::{AgentId, PermissionId, SessionId};
use harness_session_store::{SessionCommitter, SessionStore, StoreError};

use crate::agent_supervisor::AgentSupervisor;
use crate::cancellation::SessionCancellation;
use crate::integration::IntegrationRegistry;
use crate::scheduler::Scheduler;
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

use runner_spawn::{spawn_runner, RunnerSpawnArgs};

// ---------------------------------------------------------------------------
// SessionRuntime
// ---------------------------------------------------------------------------

/// The top-level orchestrator for a single session.
///
/// Owns the session's identity, state, cancellation root, event bus, the
/// session's default execution backend, workspace binding, tool registry,
/// event sink, and scheduler.
///
/// # Lifecycle
///
/// * [`cancel()`](SessionRuntime::cancel) stops all agent runners but keeps
///   the event bus alive so subscribers can receive terminal events.
/// * [`shutdown()`](SessionRuntime::shutdown) stops everything including the
///   event bus — called by `close_session` and [`Drop`].
///
/// # RC-300
///
/// When a durable store is configured, the runtime owns the session's
/// authoritative [`SessionCommitter`] (shared with every runner), the
/// per-agent [`AgentProjectionTable`], and the store handle itself — so
/// [`checkpoint()`](SessionRuntime::checkpoint) can write a truthful
/// snapshot at any point (explicit requests, terminal runs, count-based
/// cadence, and clean close).
pub struct SessionRuntime {
    /// The session's unique identifier.
    pub session_id: SessionId,
    /// The session's mutable state (agents, status, root agent id).
    ///
    /// Wrapped in a [`Mutex`] so that [`mark_failed`](SessionRuntime::mark_failed)
    /// can mutate the status and error fields through `&self` (required because
    /// the supervisor task holds an `Arc<SessionRuntime>`).
    pub state: Mutex<SessionState>,
    /// Hierarchical cancellation root for this session.
    pub cancellation: SessionCancellation,
    /// The event bus that aggregates agent event streams.
    ///
    /// Stored in [`Arc`] so a clone can be moved into the background
    /// forwarding task.
    pub event_bus: Arc<SessionEventBus>,
    /// The execution backend used for LLM requests.
    pub default_backend: Arc<dyn ExecutionBackend>,
    /// The workspace binding used for file/search access (spec §6.2, §37).
    pub workspace: Arc<dyn Workspace>,
    /// Registry of available tool executors.
    pub tool_registry: Arc<dyn ToolRegistry>,
    /// Sink for fully-enveloped agent events.
    pub event_sink: Arc<dyn EventSink>,
    /// Concurrency throttle for this session's backend/tool/process requests.
    pub scheduler: Arc<Scheduler>,
    /// Hierarchical supervisor used by every runner in this session.
    pub agent_supervisor: AgentSupervisor,
    /// Factories used to resolve explicit child backend policies.
    pub integrations: Arc<IntegrationRegistry>,
    /// Live per-agent status/usage projection, updated by every `AgentRunner`.
    live_state: LiveStateTable,
    /// Live per-agent durable projection (RC-302), updated by every runner
    /// and consumed by `checkpoint()`.
    projection: AgentProjectionTable,
    /// The session's authoritative commit boundary (RC-301), when a store is
    /// configured.
    committer: Option<Arc<SessionCommitter>>,
    /// The durable store, retained for checkpoints (RC-302).
    session_store: Option<Arc<dyn SessionStore>>,
    /// Sender to the root agent's task command channel.
    root_agent_tx: mpsc::Sender<AgentCommand>,
    /// Handle to the root agent's spawned task, captured so that a supervisor
    /// can observe completion or panic.  Wrapped in a [`Mutex<Option>`] so it
    /// can be taken exactly once from an `&self` method.
    root_task_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Independent cancellation token for the event bus background task.
    ///
    /// Kept separate from [`cancellation`] so that a session cancel does
    /// **not** shut down the bus before subscribers can receive terminal
    /// events (e.g. `Completed { Cancelled }`).  Only
    /// [`shutdown()`](SessionRuntime::shutdown) and [`Drop`] cancel this
    /// token.
    bus_cancel: CancellationToken,
}

impl SessionRuntime {
    /// Cancel only the root agent's active run.
    ///
    /// This is intentionally mailbox-scoped rather than using the session
    /// cancellation root: `AgentCommand::Cancel` cancels the active backend,
    /// tools, and children through their run-scoped tokens, while the
    /// long-lived root runner remains available to process queued follow-ups
    /// and future prompts.
    pub async fn cancel_run(&self) -> Result<(), SessionError> {
        self.root_agent_tx
            .send(AgentCommand::Cancel)
            .await
            .map_err(|_| SessionError::ChannelClosed)
    }

    /// Send steering input to the root agent.
    ///
    /// Delivery is serialized through the root agent mailbox, preserving
    /// transcript ordering with backend and tool events.
    pub async fn steer(&self, input: UserInput) -> Result<(), SessionError> {
        self.root_agent_tx
            .send(AgentCommand::Steer { input })
            .await
            .map_err(|_| SessionError::ChannelClosed)
    }

    /// Queue a follow-up prompt for the root agent.
    pub async fn follow_up(&self, input: UserInput) -> Result<(), SessionError> {
        self.root_agent_tx
            .send(AgentCommand::FollowUp { input })
            .await
            .map_err(|_| SessionError::ChannelClosed)
    }

    /// Resolve a pending root-agent permission request.
    pub async fn resolve_permission(
        &self,
        id: PermissionId,
        decision: PermissionDecision,
    ) -> Result<(), SessionError> {
        self.root_agent_tx
            .send(AgentCommand::PermissionResolved { id, decision })
            .await
            .map_err(|_| SessionError::ChannelClosed)
    }

    /// Take the root task's [`JoinHandle`](tokio::task::JoinHandle), if it has not been taken already.
    ///
    /// This allows a supervisor (e.g. [`SessionManager`](crate::session_manager::SessionManager)) to await the root
    /// task and detect panics or unexpected terminations. The handle can only
    /// be taken once; subsequent calls return `None`.
    pub fn take_root_task_handle(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.root_task_handle
            .lock()
            .expect("root_task_handle mutex poisoned")
            .take()
    }

    /// Mark the session as failed with the given error message.
    ///
    /// This transitions the session's status to [`SessionStatus::Failed`] and
    /// stores the error description. It is called by the supervisor task when
    /// the root task's `JoinHandle` produces a [`JoinError`](tokio::task::JoinError).
    pub fn mark_failed(&self, error_msg: String) {
        let mut state = self.state.lock().expect("state mutex poisoned");
        state.status = SessionStatus::Failed;
        state.error = Some(error_msg);
    }

    /// Spawn an agent runner, register its event stream with the session bus,
    /// and return the runner's command sender.
    ///
    /// This is the common lifecycle path for additional agents.  The root
    /// runner is created inline during construction so its sender can be
    /// retained by the runtime; later agents use this method.
    pub fn spawn_agent_runner(&self, agent: Agent) -> mpsc::Sender<AgentCommand> {
        let is_root = agent.parent_id.is_none();
        let (command_tx, _join) = spawn_runner(RunnerSpawnArgs {
            agent,
            is_root,
            backend: self.default_backend.clone(),
            tool_registry: self.tool_registry.clone(),
            workspace: self.workspace.clone(),
            event_sink: self.event_sink.clone(),
            scheduler: self.scheduler.clone(),
            cancellation: self.cancellation.clone(),
            agent_supervisor: self.agent_supervisor.clone(),
            integrations: self.integrations.clone(),
            live_state: self.live_state.clone(),
            projection: self.projection.clone(),
            event_bus: self.event_bus.clone(),
            session_store: self.session_store.clone(),
            committer: self.committer.clone(),
        });
        command_tx
    }

    /// Send a command to the root agent's task channel.
    ///
    /// In this Phase 2 implementation, only the root agent is supported.
    ///
    /// When a [`SessionCommand::Prompt`] is sent, the session-level status
    /// transitions to [`SessionStatus::Running`] optimistically (the agent
    /// runner processes the `StartRun` asynchronously).  This ensures that
    /// [`state_snapshot`](SessionRuntime::state_snapshot) returns an accurate
    /// active status from the moment the command is issued.
    pub async fn send_command(&self, command: SessionCommand) -> Result<(), SessionError> {
        let agent_command = match command {
            SessionCommand::Prompt(input) => {
                if self.cancellation.is_cancelled() {
                    return Err(SessionError::Cancelled);
                }
                // Optimistically transition to Running so that state_snapshot
                // reflects the session as active immediately.
                let mut state = self.state.lock().expect("state mutex poisoned");
                if matches!(
                    state.status,
                    SessionStatus::Idle | SessionStatus::Completed | SessionStatus::Cancelled
                ) {
                    state.status = SessionStatus::Running;
                    state.error = None;
                }
                drop(state);
                AgentCommand::StartRun { input }
            }
            SessionCommand::SpawnChild(spec) => AgentCommand::SpawnChild { spec },
            SessionCommand::Cancel => AgentCommand::Cancel,
            SessionCommand::Pause => AgentCommand::Pause,
            SessionCommand::Resume => AgentCommand::Resume,
            SessionCommand::ConfigureExecution(params) => {
                AgentCommand::ConfigureExecution { params }
            }
        };

        self.root_agent_tx
            .send(agent_command)
            .await
            .map_err(|_| SessionError::ChannelClosed)
    }

    /// Cancel the entire session.
    ///
    /// Triggers the hierarchical cancellation root (stopping all agent
    /// runners and spawned backend/tool tasks) and sends
    /// [`AgentCommand::Cancel`] to the root agent.
    ///
    /// The session status immediately transitions to [`SessionStatus::Cancelled`]
    /// (this is an optimistic update — cancellation is deterministic once
    /// the token fires, even though the agent runner processes it
    /// asynchronously).
    ///
    /// The event bus is **not** shut down by this call — subscribers can
    /// still receive terminal events (e.g. `Completed { Cancelled }`) after
    /// cancellation returns.
    pub async fn cancel(&self) {
        self.cancellation.cancel();
        let _ = self.root_agent_tx.send(AgentCommand::Cancel).await;

        // Optimistically transition the session-level status so that
        // state_snapshot() returns Cancelled immediately.
        let mut state = self.state.lock().expect("state mutex poisoned");
        if state.status != SessionStatus::Failed {
            state.status = SessionStatus::Cancelled;
        }
    }

    /// Shutdown the entire session, including the event bus.
    ///
    /// This is the final teardown — called by
    /// [`SessionManager::close_session`](crate::session_manager::SessionManager::close_session)
    /// and [`Drop`].  After this, no further events will be forwarded to
    /// subscribers.
    pub fn shutdown(&self) {
        self.cancellation.cancel();
        self.bus_cancel.cancel();
    }

    /// Returns a [`SessionSnapshot`] of the current session state.
    ///
    /// This is a pure read — no stored copy, just a point-in-time projection
    /// of live state.
    pub fn state_snapshot(&self) -> SessionSnapshot {
        let state = self.state.lock().expect("state mutex poisoned");
        let live = self
            .live_state
            .lock()
            .expect("live_state mutex poisoned")
            .get(&state.root_agent_id)
            .cloned()
            .unwrap_or_default();
        let status = if state.status == SessionStatus::Failed {
            SessionStatus::Failed
        } else if self.cancellation.is_cancelled() {
            SessionStatus::Cancelled
        } else {
            match live.status {
                AgentStatus::Failed => SessionStatus::Failed,
                AgentStatus::Cancelled => SessionStatus::Cancelled,
                AgentStatus::Idle if live.last_outcome == Some(AgentOutcome::Success) => {
                    SessionStatus::Completed
                }
                AgentStatus::Idle => state.status,
                _ => SessionStatus::Running,
            }
        };
        let error = state
            .error
            .clone()
            .or_else(|| live.last_error.map(|error| error.message));
        SessionSnapshot {
            session_id: self.session_id,
            status,
            root_agent_id: state.root_agent_id,
            agent_count: state.agents.len(),
            error,
        }
    }

    /// Returns the current live status/usage projection for `agent_id`.
    ///
    /// Returns [`AgentLiveState::default`] if the agent is unknown (e.g. it
    /// has not been spawned yet).
    pub fn agent_live_state(&self, agent_id: AgentId) -> AgentLiveState {
        self.live_state
            .lock()
            .expect("live_state mutex poisoned")
            .get(&agent_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Writes a truthful snapshot of the session at its last committed
    /// durable sequence (RC-302).
    ///
    /// The snapshot is built from the live per-agent projections (never a
    /// stale construction-time copy), versioned with the current
    /// [`SCHEMA_VERSION`](harness_session_store::SCHEMA_VERSION), and
    /// recorded with the workspace identity and integration references it
    /// was taken under (RC-304). Returns `Ok(())` when no store is
    /// configured (nothing to checkpoint).
    pub async fn checkpoint(&self) -> Result<(), StoreError> {
        let Some(store) = &self.session_store else {
            return Ok(());
        };
        // The snapshot point is the store's own last committed sequence —
        // authoritative and gap-free from the committer's perspective.
        let sequence = store.current_sequence(self.session_id).await?;
        let root_agent_id = self
            .state
            .lock()
            .expect("state mutex poisoned")
            .root_agent_id;
        let snapshot = projection::build_snapshot(
            self.session_id,
            root_agent_id,
            &self.projection,
            self.workspace.as_ref(),
            sequence,
            false,
            0,
        );
        store.save_snapshot(snapshot).await
    }
}

impl Drop for SessionRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}
