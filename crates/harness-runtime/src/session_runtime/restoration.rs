//! Restore-time construction: rebuilds a session runtime from a durable
//! snapshot's stored agent states. The fresh-session counterpart lives in
//! [`super::construction`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use harness_core::agent::Agent;
use harness_core::agent_state::{AgentState, PendingToolCall};
use harness_protocol::ids::SessionId;
use harness_session_store::{SessionCommitter, SessionStore, StoredAgentState};

use crate::agent_supervisor::AgentSupervisor;
use crate::cancellation::SessionCancellation;
use crate::integration::IntegrationRegistry;
use crate::scheduler::Scheduler;
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

use super::checkpoint::RuntimeCheckpointRequester;
use super::event_bus::SessionEventBus;
use super::live_state::LiveStateTable;
use super::projection::{
    capabilities_from_value, stored_agent_state, usage_from_value, AgentProjectionTable,
};
use super::runner_spawn::{spawn_runner, RunnerSpawnArgs};
use super::types::SessionState;
use super::{SessionRuntime, SessionStatus};

impl SessionRuntime {
    /// Rebuilds a session runtime from a durable snapshot's stored agent
    /// states.
    ///
    /// This is the restore-time counterpart of
    /// [`new_with_scheduler`](super::SessionRuntime::new_with_scheduler):
    /// instead of creating a fresh root agent, it reconstructs every agent
    /// (root + descendants) from its
    /// [`harness_session_store::StoredAgentState`] projection and spawns a
    /// runner for each, mirroring the live-session lifecycle (hierarchical
    /// cancellation, event bus, supervisor token registration, live-state
    /// table, per-agent task channels).
    ///
    /// The root agent is identified as the stored agent with no parent; its
    /// runner's command channel becomes the session's root command sender and
    /// its `JoinHandle` is stashed so [`SessionManager`](crate::session_manager::SessionManager)
    /// can supervise it, exactly like
    /// [`new_with_scheduler`](super::SessionRuntime::new_with_scheduler).
    /// The session starts in [`SessionStatus::Idle`] — a restored session
    /// accepts new commands rather than resuming a mid-flight run.
    ///
    /// When `session_store` is `Some`, the handle is threaded into every
    /// restored runner via
    /// [`AgentRunner::with_session_store`](crate::agent_runner::AgentRunner::with_session_store)
    /// so `Persist` effects keep flowing through the store after a restore,
    /// and the session's authoritative committer resumes after the last
    /// durable event (RC-301).
    #[allow(clippy::too_many_arguments)]
    pub fn from_stored(
        session_id: SessionId,
        stored_agents: Vec<StoredAgentState>,
        backend: Arc<dyn ExecutionBackend>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        scheduler: Arc<Scheduler>,
        integrations: Arc<IntegrationRegistry>,
        session_store: Option<Arc<dyn SessionStore>>,
    ) -> Self {
        // ── 1. Reconstruct agents from their stored projections ────────────
        let mut agents: HashMap<harness_protocol::ids::AgentId, Agent> = HashMap::new();
        let mut root_agent: Option<Agent> = None;
        for stored in stored_agents {
            let agent = Agent {
                id: stored.agent_id,
                session_id,
                parent_id: stored.parent_id,
                state: AgentState {
                    status: stored.status,
                    current_operation: stored.current_operation,
                    system_prompt: stored.system_prompt,
                    execution_params: stored.execution_params,
                    messages: stored.messages,
                    context: Default::default(),
                    active_run: stored.active_run,
                    queued_inputs: Default::default(),
                    pending_tools: stored
                        .pending_tools
                        .into_iter()
                        .map(|(call_id, pending)| {
                            (
                                call_id,
                                PendingToolCall {
                                    call: pending.call,
                                    started_at: pending.started_at,
                                },
                            )
                        })
                        .collect(),
                    pending_permissions: stored.pending_permissions,
                    children: stored.children,
                    last_error: stored.last_error,
                    transition_sequence: stored.transition_sequence,
                    depth: stored.depth,
                },
                backend: stored.backend,
                capabilities: capabilities_from_value(&stored.capabilities),
                usage: usage_from_value(&stored.usage),
                budget: stored.budget,
            };
            if agent.parent_id.is_none() {
                root_agent = Some(agent.clone());
            }
            agents.insert(agent.id, agent.clone());
        }

        let root_agent = root_agent
            .expect("a stored snapshot must contain exactly one root agent (parent_id = None)");
        let root_agent_id = root_agent.id;

        // ── 1b. RC-300: seed the projection table from the stored agents ──
        let projection: AgentProjectionTable = Arc::new(Mutex::new(HashMap::new()));
        {
            let mut table = projection.lock().expect("projection mutex poisoned");
            for agent in agents.values() {
                table.insert(agent.id, stored_agent_state(agent));
            }
        }

        // ── 2. Cancellation ──────────────────────────────────
        let cancellation = SessionCancellation::new();
        let agent_supervisor = AgentSupervisor::new(session_id, cancellation.clone());

        // ── 3. Event bus ───────────────────────────────────
        // The bus uses its own independent CancellationToken (NOT a child of
        // the session cancellation) so a session cancel leaves the bus alive
        // for terminal events, matching new_with_scheduler.
        let event_bus = Arc::new(SessionEventBus::new(256));
        let bus_handle = Arc::clone(&event_bus);
        let bus_cancel = CancellationToken::new();
        let bus_cancel_for_task = bus_cancel.clone();
        tokio::spawn(async move {
            bus_handle.run(bus_cancel_for_task).await;
        });

        // ── 4. Live state table ─────────────────────────────
        let live_state: LiveStateTable = Arc::new(Mutex::new(HashMap::new()));

        // ── 4b. RC-300: the session's authoritative committer resumes after
        // the last durable event.
        let committer = session_store.clone().map(|store| {
            let mut committer = SessionCommitter::new(store.clone(), session_id);
            committer = committer.with_checkpoint_requester(Arc::new(RuntimeCheckpointRequester {
                session_id,
                root_agent_id,
                projection: projection.clone(),
                store,
                workspace: workspace.clone(),
            }));
            Arc::new(committer)
        });

        // ── 5. Spawn one runner per restored agent ──────────
        // Each runner registers its cancellation token with the supervisor,
        // publishes live status/usage to the shared table, and — when a store
        // is configured — receives the store handle and the session's
        // committer so `Persist` effects keep flowing after the restore.
        let spawn_restored = |agent: Agent| {
            let is_root = agent.parent_id.is_none();
            spawn_runner(RunnerSpawnArgs {
                agent,
                is_root,
                backend: backend.clone(),
                tool_registry: tool_registry.clone(),
                workspace: workspace.clone(),
                event_sink: event_sink.clone(),
                scheduler: scheduler.clone(),
                cancellation: cancellation.clone(),
                agent_supervisor: agent_supervisor.clone(),
                integrations: integrations.clone(),
                live_state: live_state.clone(),
                projection: projection.clone(),
                event_bus: event_bus.clone(),
                session_store: session_store.clone(),
                committer: committer.clone(),
            })
        };

        let (root_agent_tx, root_join) = spawn_restored(root_agent);
        for agent in agents.values() {
            if agent.id == root_agent_id {
                continue;
            }
            // Non-root runners run in the background; their JoinHandles are
            // intentionally dropped, matching `spawn_agent_runner`.
            let _ = spawn_restored(agent.clone());
        }

        // ── 6. Session state ────────────────────────────────
        let session_state = SessionState {
            agents,
            root_agent_id,
            status: SessionStatus::Idle,
            error: None,
        };

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
            agent_supervisor,
            integrations,
            live_state,
            projection,
            committer,
            session_store,
            root_agent_tx,
            root_task_handle: Mutex::new(None),
            bus_cancel,
        };

        // Stash the root task handle so SessionManager can supervise it.
        *runtime
            .root_task_handle
            .lock()
            .expect("root_task_handle mutex poisoned") = Some(root_join);

        runtime
    }
}
