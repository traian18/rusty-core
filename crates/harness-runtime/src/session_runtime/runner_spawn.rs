//! Shared agent-runner spawning.
//!
//! Fresh construction ([`super::construction`]), restore
//! ([`super::restoration`]), and dynamically spawned agents
//! ([`SessionRuntime::spawn_agent_runner`](super::SessionRuntime::spawn_agent_runner))
//! all need to do the exact same thing for one agent: open its task
//! channel, derive its cancellation token and register it with the
//! supervisor, seed its live-state and durable-projection entries, build
//! and configure its [`AgentRunner`], register it with the event bus, and
//! spawn its task. Before this module existed that sequence was
//! triplicated; [`spawn_runner`] is the single place it can drift out of
//! sync from.

use std::sync::Arc;

use tokio::sync::mpsc;

use harness_core::agent::Agent;
use harness_protocol::commands::AgentCommand;
use harness_session_store::{SessionCommitter, SessionStore};

use crate::agent_runner::{AgentRunner, AgentTask};
use crate::agent_supervisor::AgentSupervisor;
use crate::cancellation::SessionCancellation;
use crate::integration::IntegrationRegistry;
use crate::scheduler::Scheduler;
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

use super::event_bus::{BridgeEventSink, SessionEventBus};
use super::live_state::{AgentLiveState, LiveStateTable};
use super::projection::{stored_agent_state, AgentProjectionTable};

/// Everything [`spawn_runner`] needs for one agent. Grouped into a struct
/// (rather than ~15 positional parameters of mostly same-shaped `Arc<dyn
/// Trait>`s) so call sites can't silently transpose two arguments of the
/// same type.
pub(super) struct RunnerSpawnArgs {
    pub(super) agent: Agent,
    /// Root runners survive individual run completion; child runners are
    /// one-shot so supervisors can observe a definitive result. See
    /// [`AgentRunner::long_lived`].
    pub(super) is_root: bool,
    pub(super) backend: Arc<dyn ExecutionBackend>,
    pub(super) tool_registry: Arc<dyn ToolRegistry>,
    pub(super) workspace: Arc<dyn Workspace>,
    pub(super) event_sink: Arc<dyn EventSink>,
    pub(super) scheduler: Arc<Scheduler>,
    pub(super) cancellation: SessionCancellation,
    pub(super) agent_supervisor: AgentSupervisor,
    pub(super) integrations: Arc<IntegrationRegistry>,
    pub(super) live_state: LiveStateTable,
    pub(super) projection: AgentProjectionTable,
    pub(super) event_bus: Arc<SessionEventBus>,
    pub(super) session_store: Option<Arc<dyn SessionStore>>,
    pub(super) committer: Option<Arc<SessionCommitter>>,
}

/// Spawns one agent's runner task and wires it into the session's shared
/// subsystems, returning its command sender and the task's `JoinHandle`.
///
/// Seeding `live_state`/`projection` here is idempotent — a caller that
/// already seeded a fresh entry for this agent (e.g. the root agent's
/// initial state, or every agent restored from a snapshot) just gets the
/// same value written again.
pub(super) fn spawn_runner(
    args: RunnerSpawnArgs,
) -> (mpsc::Sender<AgentCommand>, tokio::task::JoinHandle<()>) {
    let RunnerSpawnArgs {
        agent,
        is_root,
        backend,
        tool_registry,
        workspace,
        event_sink,
        scheduler,
        cancellation,
        agent_supervisor,
        integrations,
        live_state,
        projection,
        event_bus,
        session_store,
        committer,
    } = args;

    let agent_id = agent.id;
    let (task, _commands_tx) = AgentTask::new_with_capacities(agent_id, 64, 256);
    let command_tx = task.commands_tx.clone();
    let agent_cancel = cancellation.child_token();
    agent_supervisor.register_agent_token(agent_id, agent_cancel.clone());

    live_state
        .lock()
        .expect("live_state mutex poisoned")
        .insert(agent_id, AgentLiveState::default());
    projection
        .lock()
        .expect("projection mutex poisoned")
        .insert(agent_id, stored_agent_state(&agent));

    // Bridge to the external event sink (persistence, logging). The agent
    // runner's own emit() already sends events to task.events for the
    // session bus.
    let bridge_sink = Arc::new(BridgeEventSink {
        external_sink: Some(event_sink),
    });

    let mut runner = AgentRunner::new(
        agent,
        task,
        backend,
        tool_registry,
        workspace,
        bridge_sink,
        agent_cancel,
        live_state,
        scheduler,
    )
    .with_supervision(agent_supervisor, integrations)
    .with_projection(projection)
    .long_lived(is_root);
    if let Some(store) = session_store {
        runner = runner.with_session_store(store);
    }
    if let Some(committer) = committer {
        runner = runner.with_committer(committer);
    }

    // Register the agent's broadcast sender with the event bus so the bus
    // can read events from task.events.
    event_bus.register_agent(runner.task.id, runner.task.events.clone());

    let join = tokio::spawn(async move {
        let mut runner = runner;
        runner.run().await;
    });

    (command_tx, join)
}
