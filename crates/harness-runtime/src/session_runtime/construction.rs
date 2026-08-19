//! Fresh-session construction: builds a brand-new root agent and its
//! runner. The restore-time counterpart lives in [`super::restoration`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use harness_core::agent::Agent;
use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference,
};
use harness_protocol::ids::{AgentId, BackendId, ConfigurationId, IntegrationId};
use harness_protocol::tools::AgentToolset;
use harness_protocol::usage::AgentBudget;
use harness_session_store::{SessionCommitter, SessionStore};

use crate::agent_supervisor::AgentSupervisor;
use crate::cancellation::SessionCancellation;
use crate::integration::IntegrationRegistry;
use crate::scheduler::{Scheduler, SchedulerConfig};
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

use super::checkpoint::RuntimeCheckpointRequester;
use super::event_bus::SessionEventBus;
use super::live_state::LiveStateTable;
use super::projection::{stored_agent_state, AgentProjectionTable};
use super::runner_spawn::{spawn_runner, RunnerSpawnArgs};
use super::types::SessionState;
use super::{SessionRuntime, SessionStatus};

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
        session_id: harness_protocol::ids::SessionId,
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
        session_id: harness_protocol::ids::SessionId,
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
            None,
        )
    }

    /// Create a session runtime with an explicit scheduler and toolset.
    ///
    /// This is the most flexible constructor — it allows test code to inject
    /// a custom [`Scheduler`] with specific permit limits (e.g., for testing
    /// serialization under low concurrency).
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_scheduler(
        session_id: harness_protocol::ids::SessionId,
        backend: Arc<dyn ExecutionBackend>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        root_toolset: AgentToolset,
        scheduler: Arc<Scheduler>,
        session_store: Option<Arc<dyn SessionStore>>,
    ) -> Self {
        Self::new_with_scheduler_and_integration_id(
            session_id,
            backend,
            None,
            tool_registry,
            workspace,
            event_sink,
            root_toolset,
            scheduler,
            session_store,
        )
    }

    /// Create a session runtime whose root agent's persisted `BackendBinding`
    /// records `backend_integration_id` as its integration reference,
    /// instead of a throwaway random one.
    ///
    /// This is what makes a session restorable (RC-304): `restore_session`'s
    /// [`HostDependencyResolver`](harness_session_store::HostDependencyResolver)
    /// resolves a snapshot's `integration_references` by looking up that
    /// exact id in the live [`IntegrationRegistry`] —
    /// a random id minted here and never registered anywhere can never
    /// resolve, which is exactly what [`new_with_scheduler`](Self::new_with_scheduler)
    /// (its `backend_integration_id: None` case, below) produces. Callers
    /// that create a session through a registered integration (the only
    /// case where a *stable*, restart-durable id is actually known) should
    /// use this constructor and pass that id through; callers that inject a
    /// bare [`ExecutionBackend`] with no registered identity have no stable
    /// id to give it, so `new_with_scheduler` documents that path as not
    /// restorable — a random id there is at least honestly never resolvable,
    /// rather than silently reusing another session's factory by accident.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_scheduler_and_integration_id(
        session_id: harness_protocol::ids::SessionId,
        backend: Arc<dyn ExecutionBackend>,
        backend_integration_id: Option<IntegrationId>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        root_toolset: AgentToolset,
        scheduler: Arc<Scheduler>,
        session_store: Option<Arc<dyn SessionStore>>,
    ) -> Self {
        let integration_id = backend_integration_id.unwrap_or_default();
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
            0,
            String::new(),
            BackendBinding {
                reference: BackendReference {
                    integration: integration_id,
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
                can_spawn_agents: true,
                max_child_depth: Some(8),
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
        let agent_supervisor = AgentSupervisor::new(session_id, cancellation.clone());
        let integrations = Arc::new(IntegrationRegistry::new());

        // ── 3b. RC-300: projection table + authoritative committer ────────
        let projection: AgentProjectionTable = Arc::new(Mutex::new(HashMap::new()));
        projection
            .lock()
            .expect("projection mutex poisoned")
            .insert(root_agent_id, stored_agent_state(&root_agent));

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

        // ── 6. Agent task + runner ───────────────────────────
        let (root_agent_tx, root_join) = spawn_runner(RunnerSpawnArgs {
            agent: root_agent,
            is_root: true,
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
