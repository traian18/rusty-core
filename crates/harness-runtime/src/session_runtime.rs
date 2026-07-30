//! Session runtime — event bus, session state, and lifecycle.
//!
//! This module provides [`SessionEventBus`] — the per-session event aggregator
//! that merges event streams from all agent runners into a single ordered stream
//! for subscribers — and [`SessionRuntime`], the top-level orchestrator that
//! owns a session's root agent, cancellation scope, backend bindings, workspace
//! binding, and event bus.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use harness_core::agent::Agent;
use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
use harness_core::usage::AgentUsageSummary as CoreAgentUsageSummary;
use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference,
};
use harness_protocol::commands::{AgentCommand, AgentError, AgentOperation, AgentStatus, UserInput};
use harness_protocol::events::{AgentEventEnvelope, AgentOutcome};
use harness_protocol::ids::{
    AgentId, BackendId, ConfigurationId, IntegrationId, SessionId,
};
use harness_protocol::tools::AgentToolset;
use harness_protocol::usage::AgentBudget;
use rust_decimal::Decimal;

use crate::agent_runner::{AgentRunner, AgentTask};
use crate::cancellation::SessionCancellation;
use crate::scheduler::{Scheduler, SchedulerConfig};
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

// ---------------------------------------------------------------------------
// SessionStatus
// ---------------------------------------------------------------------------

/// The high-level status of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionStatus {
    /// Session is ready to accept commands.
    Idle,
    /// A run is in progress on at least one agent.
    Running,
    /// All agent runs have completed successfully.
    Completed,
    /// The session was cancelled before all runs completed.
    Cancelled,
    /// The root task panicked or encountered an unrecoverable error.
    Failed,
}

// ---------------------------------------------------------------------------
// SessionCommand
// ---------------------------------------------------------------------------

/// A command sent to a session to drive its execution.
///
/// These are the user-facing commands that the session translates into
/// [`AgentCommand`]s for the appropriate agent runner.
#[derive(Debug, Clone)]
pub enum SessionCommand {
    /// Start a new run with the given user input.
    Prompt(UserInput),
    /// Cancel the entire session.
    Cancel,
    /// Pause the session.
    Pause,
    /// Resume a paused session.
    Resume,
}

// ---------------------------------------------------------------------------
// SessionError
// ---------------------------------------------------------------------------

/// Errors that can occur during session operations.
#[derive(Debug, Clone)]
pub enum SessionError {
    /// The session was not found.
    SessionNotFound,
    /// The session is in an invalid state for the requested operation.
    InvalidState {
        /// The expected state.
        expected: String,
        /// The actual state.
        actual: String,
    },
    /// The operation was cancelled.
    Cancelled,
    /// A command channel was closed unexpectedly.
    ChannelClosed,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::SessionNotFound => write!(f, "session not found"),
            SessionError::InvalidState { expected, actual } => {
                write!(f, "invalid state: expected {expected}, actual {actual}")
            }
            SessionError::Cancelled => write!(f, "operation cancelled"),
            SessionError::ChannelClosed => write!(f, "command channel closed"),
        }
    }
}

impl std::error::Error for SessionError {}

// ---------------------------------------------------------------------------
// SessionSnapshot
// ---------------------------------------------------------------------------

/// A lightweight read projection of the session's current state.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    /// The session's unique identifier.
    pub session_id: SessionId,
    /// The session's current status.
    pub status: SessionStatus,
    /// The root agent's identifier.
    pub root_agent_id: AgentId,
    /// The number of agents in the session.
    pub agent_count: usize,
    /// An optional error message, populated when the session is [`SessionStatus::Failed`].
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// SessionState
// ---------------------------------------------------------------------------

/// The durable state of a session.
#[derive(Debug, Clone)]
pub struct SessionState {
    /// All agents in this session, indexed by their [`AgentId`].
    pub agents: HashMap<AgentId, Agent>,
    /// The identifier of the root agent (created when the session starts).
    pub root_agent_id: AgentId,
    /// The session's current status.
    pub status: SessionStatus,
    /// An optional error message, populated when the session enters [`SessionStatus::Failed`].
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// AgentLiveState — live read projection of one agent's runtime status
// ---------------------------------------------------------------------------

/// A live, continuously-updated projection of one agent's runtime status,
/// operation, outcome, and usage.
///
/// Every [`AgentRunner`] publishes its current state here immediately after
/// each [`Agent::apply`](harness_core::agent::Agent) transition, so
/// [`SessionRuntime::agent_live_state`] always returns a pure read of live
/// state rather than a stale copy captured at construction time.
#[derive(Debug, Clone)]
pub struct AgentLiveState {
    /// The agent's current [`AgentStatus`].
    pub status: AgentStatus,
    /// What the agent is currently doing, if anything more specific than
    /// `status` alone conveys.
    pub current_operation: Option<AgentOperation>,
    /// The outcome of the most recently completed run, if any.
    ///
    /// `AgentStatus` returns to `Idle` after a successful run completes, so
    /// this field is the only durable signal that a run finished
    /// successfully versus never having run at all.
    pub last_outcome: Option<AgentOutcome>,
    /// The most recent unrecoverable error, if any.
    pub last_error: Option<AgentError>,
    /// Self/descendant/inclusive token usage aggregated from the agent's
    /// usage ledger.
    pub usage: CoreAgentUsageSummary,
    /// Total number of usage records recorded so far (approximates request
    /// count for Phase 2's single-backend-call-per-record model).
    pub total_requests: u64,
    /// Total cost across all usage records that reported one, if any
    /// reported a cost at all.
    pub total_cost_usd: Option<Decimal>,
}

impl Default for AgentLiveState {
    fn default() -> Self {
        Self {
            status: AgentStatus::Idle,
            current_operation: None,
            last_outcome: None,
            last_error: None,
            usage: CoreAgentUsageSummary::default(),
            total_requests: 0,
            total_cost_usd: None,
        }
    }
}

/// Shared table of per-agent [`AgentLiveState`], written by [`AgentRunner`]s
/// and read by [`SessionRuntime::agent_live_state`] / `SessionClient::snapshot`.
pub type LiveStateTable = Arc<Mutex<HashMap<AgentId, AgentLiveState>>>;

// ---------------------------------------------------------------------------
// BridgeEventSink — connects EventSink trait to external persistence/logging
// ---------------------------------------------------------------------------

/// An [`EventSink`] implementation that forwards events to an optional
/// external sink (e.g. persistence, logging).
///
/// Note: The agent runner's [`emit`](crate::agent_runner::AgentRunner::emit)
/// already delivers events directly to the agent's `task.events` broadcast
/// channel (which the [`SessionEventBus`] polls). This bridge only forwards
/// to an optional secondary sink — it does NOT duplicate events back into
/// `task.events`.
struct BridgeEventSink {
    /// An optional external sink for persistence / logging.
    external_sink: Option<Arc<dyn EventSink>>,
}

impl EventSink for BridgeEventSink {
    fn send(&self, envelope: AgentEventEnvelope) {
        if let Some(ref sink) = self.external_sink {
            sink.send(envelope);
        }
    }
}

// ---------------------------------------------------------------------------
// SessionEventBus
// ---------------------------------------------------------------------------

/// Aggregates event broadcasts from all agent runners in a session and fans
/// them out to external subscribers as a single ordered stream.
///
/// # Ordering
///
/// Every event that flows through the bus receives a monotonically increasing
/// [`session_sequence`](AgentEventEnvelope::session_sequence) number.  This,
/// combined with the agent-local [`agent_sequence`](AgentEventEnvelope::agent_sequence),
/// allows consumers to reconstruct a total order across all agents in the
/// session.
///
/// # Lifecycle
///
/// 1. Create with [`SessionEventBus::new`].
/// 2. As each agent runner is spawned, call
///    [`register_agent`](SessionEventBus::register_agent) with the broadcast
///    sender from that runner's [`AgentTask`].
/// 3. Spawn the [`run`](SessionEventBus::run) loop as a background tokio task.
/// 4. External consumers obtain a receiver via
///    [`subscribe`](SessionEventBus::subscribe).
pub struct SessionEventBus {
    /// Broadcast receivers for each registered agent. Wrapped in a [`Mutex`]
    /// so that agents can be registered from any thread while the run loop
    /// polls them concurrently.
    receivers: Mutex<Vec<broadcast::Receiver<AgentEventEnvelope>>>,

    /// Shared sender for external subscribers carved from the broadcast
    /// channel created in [`new`](SessionEventBus::new).
    subscriber_sender: broadcast::Sender<AgentEventEnvelope>,

    /// Monotonically increasing session-level sequence counter. Every event
    /// that flows through the bus receives the next value before being fanned
    /// out.
    session_sequence: AtomicU64,
}

impl SessionEventBus {
    /// Create a new event bus with the given subscriber channel capacity.
    pub fn new(capacity: usize) -> Self {
        let (subscriber_sender, _) = broadcast::channel(capacity);
        Self {
            receivers: Mutex::new(Vec::new()),
            subscriber_sender,
            session_sequence: AtomicU64::new(0),
        }
    }

    /// Register a new agent's event source with the bus.
    ///
    /// The `sender` should be the [`broadcast::Sender`] from the agent's
    /// [`AgentTask::events`]. The bus creates a receiver and will poll it
    /// in the [`run`](SessionEventBus::run) loop.
    pub fn register_agent(
        &self,
        _agent_id: AgentId,
        sender: broadcast::Sender<AgentEventEnvelope>,
    ) {
        let rx = sender.subscribe();
        self.receivers
            .lock()
            .expect("receivers mutex poisoned")
            .push(rx);
    }

    /// Return a new subscriber receiver that will receive all session events.
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEventEnvelope> {
        self.subscriber_sender.subscribe()
    }

    /// Run the event bus's forwarding loop.
    ///
    /// Polls every registered agent receiver via [`try_recv`],
    /// assigns a monotonically increasing `session_sequence` to each event,
    /// and forwards to all subscribers.  Exits when `bus_cancel` is
    /// triggered.
    pub async fn run(&self, bus_cancel: CancellationToken) {
        loop {
            if bus_cancel.is_cancelled() {
                // Drain any remaining events.  This is important when the
                // cancellation token fires concurrently with a session
                // cancel: the agent runner may emit its terminal event
                // before we get to poll again, and we need to forward it.
                self.poll_receivers();
                break;
            }

            // poll_receivers is synchronous — the mutex guard is scoped
            // inside it and is released before we reach any await point.
            let had_data = self.poll_receivers();

            if !had_data {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            } else {
                tokio::task::yield_now().await;
            }
        }
    }

    /// Poll all registered receivers once, forwarding events to subscribers.
    ///
    /// Returns `true` if at least one event was forwarded.
    ///
    /// The mutex guard is scoped to this function so that the lock is
    /// not held across any await point in the caller.
    fn poll_receivers(&self) -> bool {
        let mut had_data = false;

        let mut receivers = self
            .receivers
            .lock()
            .expect("receivers mutex poisoned");

        // Drain all available events from each receiver.
        // `retain_mut` returns `true` to keep the receiver, `false` to
        // remove it when the sender has been dropped.
        receivers.retain_mut(|rx| loop {
            match rx.try_recv() {
                Ok(mut envelope) => {
                    let seq = self.session_sequence.fetch_add(1, Ordering::Relaxed);
                    envelope.session_sequence = Some(seq);
                    let _ = self.subscriber_sender.send(envelope);
                    had_data = true;
                    // Continue draining this receiver.
                }
                Err(broadcast::error::TryRecvError::Empty) => {
                    // No more events from this receiver for now.
                    break true;
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    // Receiver was dropped; remove it.
                    break false;
                }
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    tracing::warn!("Session event bus lagged by {n} events");
                    // Continue draining; the channel is still live.
                }
            }
        });

        had_data
    }
}

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
///   event bus — called by `close_session` and `Drop`.
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
    /// Live per-agent status/usage projection, updated by every `AgentRunner`.
    live_state: LiveStateTable,
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
    /// Create a new session runtime with a default scheduler.
    ///
    /// This constructor:
    ///
    /// 1. Creates a root [`Agent`] with empty capabilities.
    /// 2. Creates a [`SessionCancellation`] for hierarchical cancellation.
    /// 3. Creates a [`SessionEventBus`] and spawns its background forwarding
    ///    task.
    /// 4. Spawns the root agent's runner task.
    /// 5. Returns a fully initialised `SessionRuntime`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: SessionId,
        backend: Arc<dyn ExecutionBackend>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
    ) -> Self {
        Self::new_with_toolset(
            session_id,
            backend,
            tool_registry,
            workspace,
            event_sink,
            AgentToolset {
                tools: HashMap::new(),
            },
        )
    }

    /// Create a session runtime whose root agent receives the supplied tool
    /// capabilities, using a default scheduler.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_toolset(
        session_id: SessionId,
        backend: Arc<dyn ExecutionBackend>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        root_toolset: AgentToolset,
    ) -> Self {
        let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));
        Self::new_with_scheduler(
            session_id,
            backend,
            tool_registry,
            workspace,
            event_sink,
            root_toolset,
            scheduler,
        )
    }

    /// Create a session runtime with an explicit scheduler and toolset.
    ///
    /// This is the most flexible constructor — it allows test code to inject
    /// a custom [`Scheduler`] with specific permit limits (e.g., for testing
    /// serialization under low concurrency).
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_scheduler(
        session_id: SessionId,
        backend: Arc<dyn ExecutionBackend>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        root_toolset: AgentToolset,
        scheduler: Arc<Scheduler>,
    ) -> Self {
        let enabled_tool_names: Vec<_> = root_toolset
            .enabled_descriptors()
            .into_iter()
            .map(|descriptor| descriptor.name.as_str())
            .collect();
        let workspace_capabilities = WorkspaceCapabilities {
            can_read: enabled_tool_names.contains(&"fs.read"),
            can_write: enabled_tool_names.contains(&"fs.edit"),
            can_search: enabled_tool_names.contains(&"workspace.search"),
        };

        // ── 1. Root agent ───────────────────────────────────
        let root_agent_id = AgentId::new();
        let root_agent = Agent::new(
            root_agent_id,
            session_id,
            None,
            String::new(),
            BackendBinding {
                reference: BackendReference {
                    integration: IntegrationId::new(),
                    configuration: ConfigurationId::new(),
                    model: None,
                },
                descriptor: BackendDescriptor {
                    id: BackendId::new(),
                    name: backend.descriptor().name,
                    description: backend.descriptor().description,
                    capabilities: backend.capabilities(),
                },
            },
            AgentCapabilities {
                tools: root_toolset,
                can_spawn_agents: false,
                max_child_depth: None,
                workspace: workspace_capabilities,
                backend: BackendCapabilities::default(),
            },
            AgentBudget::default(),
        );

        // ── 2. Session state ────────────────────────────────
        let mut agents = HashMap::new();
        agents.insert(root_agent_id, root_agent.clone());
        let session_state = SessionState {
            agents,
            root_agent_id,
            status: SessionStatus::Idle,
            error: None,
        };

        // ── 3. Cancellation ──────────────────────────────────
        let cancellation = SessionCancellation::new();

        // ── 4. Event bus ───────────────────────────────────
        // NOTE: The event bus uses its own independent CancellationToken,
        // NOT a child of the session cancellation.  This way, calling
        // cancel() stops agent runners but leaves the bus alive so
        // subscribers can receive terminal events before shutdown.
        let event_bus = Arc::new(SessionEventBus::new(256));
        let bus_handle = Arc::clone(&event_bus);
        let bus_cancel = CancellationToken::new();
        let bus_cancel_for_task = bus_cancel.clone();
        tokio::spawn(async move {
            bus_handle.run(bus_cancel_for_task).await;
        });

        // ── 5. Live state table ─────────────────────────────
        let live_state: LiveStateTable = Arc::new(Mutex::new(HashMap::new()));
        live_state
            .lock()
            .expect("live_state mutex poisoned")
            .insert(root_agent_id, AgentLiveState::default());

        // ── 6. Agent task + runner ───────────────────────────
        let agent_cancel = cancellation.child_token();
        let (task, _commands_tx) = AgentTask::new_with_capacities(root_agent_id, 64, 256);
        let root_agent_tx = task.commands_tx.clone();

        // Create a BridgeEventSink that forwards to the external
        // event_sink (persistence, logging). The agent runner's own
        // emit() already sends events to task.events for the session bus.
        let bridge_sink = Arc::new(BridgeEventSink {
            external_sink: Some(event_sink.clone()),
        });

        let runner = AgentRunner::new(
            root_agent,
            task,
            backend.clone(),
            tool_registry.clone(),
            bridge_sink,
            agent_cancel,
            live_state.clone(),
            scheduler.clone(),
        );

        // Register the agent's broadcast sender with the event bus
        // so the bus can read events from task.events.
        event_bus.register_agent(root_agent_id, runner.task.events.clone());

        // Spawn the runner task and capture its JoinHandle so a supervisor
        // can observe completion or panic.
        let root_join: tokio::task::JoinHandle<()> = tokio::spawn(async move {
            let mut runner = runner;
            runner.run().await;
        });

        // ── 7. Assemble ──────────────────────────────────
        let runtime = Self {
            session_id,
            state: Mutex::new(session_state),
            cancellation,
            event_bus,
            default_backend: backend,
            workspace,
            tool_registry,
            event_sink,
            scheduler,
            live_state,
            root_agent_tx,
            root_task_handle: Mutex::new(None),
            bus_cancel,
        };

        // Stash the root task handle so SessionManager can supervise it.
        *runtime.root_task_handle.lock().expect("root_task_handle mutex poisoned") = Some(root_join);

        runtime
    }

    /// Take the root task's [`JoinHandle`], if it has not been taken already.
    ///
    /// This allows a supervisor (e.g. [`SessionManager`]) to await the root
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
    /// the root task's `JoinHandle` produces a [`JoinError`].
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
        let (task, _commands_tx) = AgentTask::new_with_capacities(agent.id, 64, 256);
        let command_tx = task.commands_tx.clone();
        let agent_cancel = self.cancellation.child_token();

        self.live_state
            .lock()
            .expect("live_state mutex poisoned")
            .insert(agent.id, AgentLiveState::default());

        let bridge_sink = Arc::new(BridgeEventSink {
            external_sink: Some(self.event_sink.clone()),
        });

        let runner = AgentRunner::new(
            agent,
            task,
            self.default_backend.clone(),
            self.tool_registry.clone(),
            bridge_sink,
            agent_cancel,
            self.live_state.clone(),
            self.scheduler.clone(),
        );

        self.event_bus
            .register_agent(runner.task.id, runner.task.events.clone());

        tokio::spawn(async move {
            let mut runner = runner;
            runner.run().await;
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
                // Optimistically transition to Running so that state_snapshot
                // reflects the session as active immediately.
                let mut state = self.state.lock().expect("state mutex poisoned");
                if state.status == SessionStatus::Idle {
                    state.status = SessionStatus::Running;
                }
                drop(state);
                AgentCommand::StartRun { input }
            }
            SessionCommand::Cancel => AgentCommand::Cancel,
            SessionCommand::Pause => AgentCommand::Pause,
            SessionCommand::Resume => AgentCommand::Resume,
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
    /// This is the final teardown — called by [`SessionManager::close_session`]
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
        SessionSnapshot {
            session_id: self.session_id,
            status: state.status,
            root_agent_id: state.root_agent_id,
            agent_count: state.agents.len(),
            error: state.error.clone(),
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
}

impl Drop for SessionRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::broadcast;

    use harness_protocol::backend::{ExecutionEvent, ExecutionResult};
    use harness_protocol::commands::UserInput;
    use harness_protocol::events::{AgentEvent, AgentEventEnvelope, AgentOutcome};
    use harness_protocol::ids::{RequestId, SessionId};
    use harness_protocol::usage::{Cost, ModelUsage};

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

        let root_id = runtime.state.lock().expect("state mutex poisoned").root_agent_id;

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
            if batch.iter().any(|e| matches!(e, AgentEvent::Completed { .. })) {
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
            after.usage.inclusive_usage.input_tokens.value(),
            Some(5),
            "usage should be populated from the scripted ExecutionResult"
        );
        assert_eq!(after.total_requests, 1);
    }
}
