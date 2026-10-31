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
//!   `StoredAgentState` after each transition, and [`SessionRuntime::checkpoint`]
//!   (plus the automatic snapshot hooks) build versioned,
//!   dependency-recorded `DurableSessionSnapshot`s from those projections.
//!
//! # Architecture & SOLID Principles
//!
//! Each concern below lives in its own file (Single Responsibility
//! Principle) instead of one struct owning all of it:
//!
//! - **Value types (`types`)**: [`SessionStatus`], [`SessionCommand`],
//!   [`SessionError`], [`SessionSnapshot`], [`SessionState`] — plain data,
//!   no behavior.
//! - **Live status (`live_state`)**: [`AgentLiveState`] and
//!   [`LiveStateTable`], the per-agent projection every runner keeps fresh.
//! - **Durable projection (`projection`)**: [`AgentProjectionTable`] and
//!   both directions of the live-agent ⇄ `StoredAgentState` conversion
//!   (`stored_agent_state`, `build_snapshot`, and the restore-side
//!   `capabilities_from_value`/`usage_from_value`).
//! - **Event aggregation (`event_bus`)**: [`SessionEventBus`] and the
//!   `BridgeEventSink` that forwards to an external sink.
//! - **Automatic checkpoints (`checkpoint`)**: `RuntimeCheckpointRequester`,
//!   the [`CheckpointRequester`](rusty_session_store::CheckpointRequester)
//!   the committer calls into.
//! - **Runner spawning (`runner_spawn`)**: the one place that opens an
//!   agent's task channel, wires it into the supervisor/live-state/
//!   projection/event-bus, and spawns it — shared by fresh construction,
//!   restore, and dynamically spawned agents so those three lifecycles
//!   cannot drift out of sync.
//! - **Fresh construction (`construction`)**: `SessionRuntime::new*`.
//! - **Restore construction (`restoration`)**: `SessionRuntime::from_stored`.
//! - **Runtime ([`SessionRuntime`], this file)**: the struct definition and
//!   its steady-state operations (mailbox commands, cancellation, snapshots,
//!   checkpoints) — everything a session does once it is already running.

mod assembly;
mod checkpoint;
mod commands;
mod construction;
mod delegation;
mod event_bus;
mod inspect;
mod lifecycle;
mod live_state;
mod projection;
mod restoration;
mod runner_spawn;
mod types;

pub use assembly::SessionHost;
pub use event_bus::SessionEventBus;
pub use live_state::{AgentLiveState, LiveStateTable};
pub use projection::AgentProjectionTable;
pub use restoration::RestoreError;
pub use types::{SessionCommand, SessionError, SessionSnapshot, SessionState, SessionStatus};

pub(crate) use projection::stored_agent_state;

#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use rusty_protocol::commands::AgentCommand;

use rusty_protocol::ids::SessionId;
use rusty_session_store::{SessionCommitter, SessionStore};

use crate::agent_supervisor::AgentSupervisor;
use crate::cancellation::SessionCancellation;
use crate::integration::IntegrationRegistry;
use crate::scheduler::Scheduler;
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

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
    /// Live sessions this one delegates work to (orchestration steps run in
    /// isolated sessions sharing its backend and tools), so the host can
    /// still reach them -- e.g. to change a step's model mid-run.
    delegated: Mutex<std::collections::HashMap<SessionId, std::sync::Weak<SessionRuntime>>>,
}

impl SessionRuntime {}

impl Drop for SessionRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}
