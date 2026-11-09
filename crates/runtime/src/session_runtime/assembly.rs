//! Assembling a session: the parts every session has, however it starts.
//!
//! A fresh session ([`super::construction`]) and a restored one ([`super::restoration`]) differ
//! only in the agents they begin with. Everything else (cancellation, supervisor, event bus, live
//! state, projection, committer, one runner per agent) is built here, so the two cannot drift.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use rusty_agent_core::agent::Agent;
use rusty_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
    ExecutionResult,
};
use rusty_protocol::commands::AgentCommand;
use rusty_protocol::ids::{AgentId, SessionId};
use rusty_session_store::{SessionCommitter, SessionStore};

use crate::agent_supervisor::AgentSupervisor;
use crate::cancellation::SessionCancellation;
use crate::integration::IntegrationRegistry;
use crate::locks::MutexExt;
use crate::scheduler::{Scheduler, SchedulerConfig};
use crate::traits::{
    EventSink, ExecutionBackend, RequestObservationBindings, ToolRegistry, Workspace,
};

use super::checkpoint::RuntimeCheckpointRequester;
use super::event_bus::SessionEventBus;
use super::live_state::LiveStateTable;
use super::projection::{stored_agent_state, AgentProjectionTable};
use super::runner_spawn::{spawn_runner, RunnerSpawnArgs};
use super::types::SessionState;
use super::{SessionRuntime, SessionStatus};

struct SessionBackend {
    inner: Arc<dyn ExecutionBackend>,
    bindings: RequestObservationBindings,
}

#[async_trait]
impl ExecutionBackend for SessionBackend {
    fn descriptor(&self) -> BackendDescriptor {
        self.inner.descriptor()
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }

    fn observation_bindings(&self) -> Option<RequestObservationBindings> {
        Some(self.bindings.clone())
    }

    async fn execute(
        &self,
        request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        self.inner.execute(request, sink, cancel).await
    }
}

/// The host services a session runs on, shared by every agent in it.
pub struct SessionHost {
    pub session_id: SessionId,
    pub backend: Arc<dyn ExecutionBackend>,
    pub tool_registry: Arc<dyn ToolRegistry>,
    pub workspace: Arc<dyn Workspace>,
    pub event_sink: Arc<dyn EventSink>,
    /// Throttles the session's backend, tool and process requests.
    pub scheduler: Arc<Scheduler>,
    /// Factories used to resolve explicit child backend policies.
    pub integrations: Arc<IntegrationRegistry>,
    /// The durable store; without one the session is not persisted.
    pub session_store: Option<Arc<dyn SessionStore>>,
}

impl SessionHost {
    /// A host with a default scheduler, no integrations and no store.
    pub fn new(
        session_id: SessionId,
        backend: Arc<dyn ExecutionBackend>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            session_id,
            backend,
            tool_registry,
            workspace,
            event_sink,
            scheduler: Arc::new(Scheduler::new(SchedulerConfig::default())),
            integrations: Arc::new(IntegrationRegistry::new()),
            session_store: None,
        }
    }
}

impl SessionRuntime {
    /// Builds a session around its root agent and the others, and starts a runner for each. The
    /// session starts idle: it accepts new commands rather than resuming a run.
    pub(super) fn assemble(host: SessionHost, root: Agent, others: Vec<Agent>) -> Self {
        let session_id = host.session_id;
        let root_agent_id = root.id;
        let agents: Vec<Agent> = std::iter::once(root.clone())
            .chain(others.iter().cloned())
            .collect();
        let projection = seed_projection(&agents);
        let committer = committer_for(&host, root_agent_id, &projection);
        let cancellation = SessionCancellation::new();
        let agent_supervisor = AgentSupervisor::new(session_id, cancellation.clone());
        let (event_bus, bus_cancel) = start_event_bus();
        let live_state: LiveStateTable = Arc::new(Mutex::new(HashMap::new()));
        let observation_bindings = host.backend.observation_bindings().unwrap_or_default();
        let backend: Arc<dyn ExecutionBackend> = Arc::new(SessionBackend {
            inner: host.backend.clone(),
            bindings: observation_bindings.clone(),
        });

        let spawn = |agent: Agent| {
            let is_root = agent.id == root_agent_id;
            spawn_runner(RunnerSpawnArgs {
                agent,
                is_root,
                backend: backend.clone(),
                tool_registry: host.tool_registry.clone(),
                workspace: host.workspace.clone(),
                event_sink: host.event_sink.clone(),
                scheduler: host.scheduler.clone(),
                cancellation: cancellation.clone(),
                agent_supervisor: agent_supervisor.clone(),
                integrations: host.integrations.clone(),
                live_state: live_state.clone(),
                projection: projection.clone(),
                event_bus: event_bus.clone(),
                session_store: host.session_store.clone(),
                committer: committer.clone(),
            })
        };
        let (root_agent_tx, root_join) = spawn_all(root, &others, spawn);

        let state = SessionState {
            agents: agents.into_iter().map(|agent| (agent.id, agent)).collect(),
            root_agent_id,
            status: SessionStatus::Idle,
            error: None,
        };
        let runtime = Self {
            session_id,
            state: Mutex::new(state),
            cancellation,
            event_bus,
            default_backend: backend,
            workspace: host.workspace,
            tool_registry: host.tool_registry,
            event_sink: host.event_sink,
            scheduler: host.scheduler,
            agent_supervisor,
            integrations: host.integrations,
            live_state,
            projection,
            committer,
            session_store: host.session_store,
            root_agent_tx,
            root_task_handle: Mutex::new(None),
            bus_cancel,
            delegated: Mutex::new(Default::default()),
        };
        // Stash the root task handle so the session manager can supervise it.
        *runtime.root_task_handle.lock_or_recover() = Some(root_join);
        runtime
    }
}

/// The durable projection of every starting agent.
fn seed_projection(agents: &[Agent]) -> AgentProjectionTable {
    let projection: AgentProjectionTable = Arc::new(Mutex::new(HashMap::new()));
    {
        let mut table = projection.lock_or_recover();
        for agent in agents {
            table.insert(agent.id, stored_agent_state(agent));
        }
    }
    projection
}

/// The session's authoritative commit boundary, when a store is configured. It resumes after the
/// last durable event.
fn committer_for(
    host: &SessionHost,
    root_agent_id: AgentId,
    projection: &AgentProjectionTable,
) -> Option<Arc<SessionCommitter>> {
    let store = host.session_store.clone()?;
    let requester = RuntimeCheckpointRequester {
        session_id: host.session_id,
        root_agent_id,
        projection: projection.clone(),
        store: store.clone(),
        workspace: host.workspace.clone(),
    };
    let committer = SessionCommitter::new(store, host.session_id)
        .with_checkpoint_requester(Arc::new(requester));
    Some(Arc::new(committer))
}

/// Starts the bus's forwarding task. The bus has its own cancellation token, not a child of the
/// session's: cancelling the session stops the agent runners but leaves the bus alive so
/// subscribers can receive terminal events before shutdown.
fn start_event_bus() -> (Arc<SessionEventBus>, CancellationToken) {
    let bus = Arc::new(SessionEventBus::new(256));
    let cancel = CancellationToken::new();
    let (forwarding, stop) = (bus.clone(), cancel.clone());
    tokio::spawn(async move {
        forwarding.run(stop).await;
    });
    (bus, cancel)
}

/// Spawns the root runner and then every other one; returns the root's channel and task. The other
/// runners run in the background and their handles are intentionally dropped.
fn spawn_all(
    root: Agent,
    others: &[Agent],
    spawn: impl Fn(Agent) -> (mpsc::Sender<AgentCommand>, tokio::task::JoinHandle<()>),
) -> (mpsc::Sender<AgentCommand>, tokio::task::JoinHandle<()>) {
    let started = spawn(root);
    for agent in others {
        let _ = spawn(agent.clone());
    }
    started
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeBackend;
    use crate::traits::{RequestObservationBinding, RequestObservationPublisher};
    use rusty_protocol::events::AgentEvent;
    use rusty_protocol::ids::{RequestId, RunId};

    struct Publisher;

    #[async_trait]
    impl RequestObservationPublisher for Publisher {
        async fn publish(&self, _: AgentEvent, _: RunId) -> bool {
            true
        }
    }

    #[test]
    fn session_backend_exposes_shared_request_keyed_bindings() {
        let bindings = RequestObservationBindings::default();
        let backend = SessionBackend {
            inner: Arc::new(FakeBackend::new()),
            bindings: bindings.clone(),
        };
        let first = backend.observation_bindings().expect("session registry");
        let second = backend.observation_bindings().expect("session registry");
        let request_id = RequestId::new();
        first.register(RequestObservationBinding {
            session_id: SessionId::new(),
            agent_id: AgentId::new(),
            parent_agent_id: None,
            run_id: RunId::new(),
            request_id,
            request_start_order: 1,
            publisher: Arc::new(Publisher),
            session_cancellation: CancellationToken::new(),
        });
        assert!(second.get(request_id).is_some());
        assert!(second.get(RequestId::new()).is_none());
        second.remove(request_id);
        assert!(bindings.get(request_id).is_none());
    }
}
