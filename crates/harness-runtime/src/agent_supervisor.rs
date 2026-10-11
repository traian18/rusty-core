//! Hierarchical supervision and child-agent spawning for one session.

use harness_core::behavior::{ChildBehaviorResolver, PolicyChildBehaviorResolver};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;

use harness_core::agent::Agent;
use harness_core::budget::{BudgetCheck, BudgetError};
use harness_core::capabilities::{AgentCapabilities, CapabilityError};
use harness_protocol::backend::{BackendBinding, BackendDescriptor, BackendReference};
use harness_protocol::commands::{AgentCommand, AgentResult};
use harness_protocol::effects::{BackendPolicy, SpawnAgentSpec, SpawnMode, WorkspacePolicy};
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
use harness_protocol::ids::{AgentId, BackendId, EventId, SessionId, Timestamp};
use harness_protocol::usage::AgentBudget;
use harness_workspace::{ReadOnlyWorkspace, SnapshotWorkspace, WorkspaceError, WorktreeWorkspace};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::agent_runner::{AgentRunner, AgentTask};
use crate::cancellation::SessionCancellation;
use crate::integration::{IntegrationError, IntegrationRegistry};
use crate::scheduler::Scheduler;
use crate::session_runtime::LiveStateTable;
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

/// Per-child bookkeeping tracked by the supervisor.
///
/// `depth` records the child's nesting depth (used for `max_depth` accounting
/// on subsequent generations) and `cancel` is the child's dedicated
/// cancellation token (reserved for future per-child cancellation; today
/// cancellation is session-scoped via [`SessionCancellation`]).
#[allow(dead_code)]
struct AgentHandle {
    parent_id: Option<AgentId>,
    depth: u32,
    cancel: CancellationToken,
    join: JoinHandle<()>,
    commands: mpsc::Sender<AgentCommand>,
    result: oneshot::Receiver<AgentResult>,
}

/// The result of spawning according to SpawnMode.
#[derive(Debug, Clone)]
pub enum SpawnOutcome {
    /// Boxed: a child's full result is far larger than a `Detached` id.
    Awaited(Box<AgentResult>),
    Detached(AgentId),
}

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("budget: {0}")]
    Budget(#[from] BudgetError),
    #[error("capability: {0}")]
    Capability(#[from] CapabilityError),
    #[error("integration: {0}")]
    Integration(#[from] IntegrationError),
    #[error("workspace: {0}")]
    Workspace(#[from] WorkspaceError),
    #[error("parent agent not found: {0:?}")]
    ParentNotFound(AgentId),
    #[error("agent {0:?} is not allowed to spawn children")]
    SpawnNotAllowed(AgentId),
    #[error("child budget is looser than its parent for {0}")]
    BudgetEscalation(&'static str),
    #[error("child {0:?} finished without producing a result")]
    ChildResultLost(AgentId),
    #[error("child behavior: {0}")]
    ChildBehavior(#[from] harness_core::behavior::ChildPolicyError),
}

#[derive(Clone)]
pub struct AgentSupervisor {
    /// The session this supervisor is scoped to.
    #[allow(dead_code)]
    session_id: SessionId,
    session_cancel: SessionCancellation,
    agents: Arc<RwLock<HashMap<AgentId, AgentHandle>>>,
    children_of: Arc<RwLock<HashMap<AgentId, Vec<AgentId>>>>,
    child_capabilities: Arc<RwLock<HashMap<AgentId, AgentCapabilities>>>,
    child_workspaces: Arc<RwLock<HashMap<AgentId, Arc<dyn Workspace>>>>,
    agent_tokens: Arc<StdRwLock<HashMap<AgentId, CancellationToken>>>,
    /// Decides the behavior profile each spawned child starts with. Shared
    /// by every clone of this supervisor, so replacing it applies session-wide.
    child_behavior: Arc<StdRwLock<Arc<dyn ChildBehaviorResolver>>>,
}

impl AgentSupervisor {
    pub fn new(session_id: SessionId, session_cancel: SessionCancellation) -> Self {
        Self {
            session_id,
            session_cancel,
            agents: Arc::new(RwLock::new(HashMap::new())),
            children_of: Arc::new(RwLock::new(HashMap::new())),
            child_capabilities: Arc::new(RwLock::new(HashMap::new())),
            child_workspaces: Arc::new(RwLock::new(HashMap::new())),
            agent_tokens: Arc::new(StdRwLock::new(HashMap::new())),
            child_behavior: Arc::new(StdRwLock::new(Arc::new(PolicyChildBehaviorResolver))),
        }
    }

    /// Replace how children get their behavior profile (default: follow the
    /// parent profile's `children` policy, i.e. inherit).
    pub fn set_child_behavior_resolver(&self, resolver: Arc<dyn ChildBehaviorResolver>) {
        *self
            .child_behavior
            .write()
            .expect("child behavior lock poisoned") = resolver;
    }

    /// Registers an already-created agent token (notably the session root),
    /// allowing descendants to derive from the exact parent token.
    pub fn register_agent_token(&self, agent_id: AgentId, token: CancellationToken) {
        self.agent_tokens
            .write()
            .expect("agent token lock poisoned")
            .insert(agent_id, token);
    }

    /// Performs the budget checks before creating a child task.
    pub async fn max_children_depth_check(
        &self,
        parent_id: AgentId,
        parent_depth: u32,
        parent_budget: &AgentBudget,
    ) -> Result<(), SupervisorError> {
        let children = self.children_of.read().await;
        let current = children.get(&parent_id).map_or(0, |ids| ids.len()) as u32;
        parent_budget.check_children(current)?;
        parent_budget.check_depth(parent_depth)?;
        Ok(())
    }

    /// Executes the complete eight-step child spawn flow.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn_child(
        &self,
        parent: &Agent,
        parent_backend: Arc<dyn ExecutionBackend>,
        integration_registry: &IntegrationRegistry,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        scheduler: Arc<Scheduler>,
        parent_commands_tx: mpsc::Sender<AgentCommand>,
        spec: SpawnAgentSpec,
    ) -> Result<AgentId, SupervisorError> {
        let origin_tool_call_id = spec.origin_tool_call_id;
        // 1. Gate before creating a child task. The parent's real nesting depth
        //    is used so `max_depth` is enforced across the whole tree.
        let parent_depth = parent.state.depth;
        if !parent.capabilities.can_spawn_agents {
            return Err(SupervisorError::SpawnNotAllowed(parent.id));
        }
        self.max_children_depth_check(parent.id, parent_depth, &parent.budget)
            .await?;
        validate_child_budget(&parent.budget, &spec.budget)?;

        // M3: bound total concurrent agents across the process via
        // `SchedulerConfig::max_active_agents`, mirroring how
        // `SessionManager::create_session` already bounds concurrent
        // sessions. Acquired before any backend/workspace setup work so an
        // exhausted cap doesn't pay for work that will just be discarded,
        // and held for the child's entire lifetime (moved into the spawned
        // task below, released automatically when it ends).
        let agent_permit = scheduler.acquire_agent_permit().await;

        // 2. Inherit the parent backend or instantiate a fresh explicit one.
        let (backend, child_backend_reference) = match &spec.backend {
            BackendPolicy::Inherit => (parent_backend, parent.backend.reference.clone()),
            BackendPolicy::Explicit(reference) => {
                let backend = integration_registry
                    .create(
                        &reference.integration.to_string(),
                        reference_to_config(reference),
                    )
                    .await?;
                (backend, reference.clone())
            }
        };

        // 3. Derive capabilities with non-escalating tool/spawn/depth rules.
        let capabilities =
            parent
                .capabilities
                .derive_child_agent_capabilities(&spec.tools, None, None)?;

        // 4. Resolve the requested workspace policy.
        let child_id = AgentId::new();
        let child_workspace: Arc<dyn Workspace> = match &spec.workspace {
            WorkspacePolicy::Inherit => workspace.clone(),
            WorkspacePolicy::ReadOnly => Arc::new(ReadOnlyWorkspace::new(workspace.clone())),
            WorkspacePolicy::Snapshot => Arc::new(
                SnapshotWorkspace::create(workspace.root())
                    .await
                    .map_err(SupervisorError::from)?,
            ),
            WorkspacePolicy::NewWorktree => Arc::new(
                WorktreeWorkspace::create(workspace.root(), &child_id.to_string())
                    .await
                    .map_err(SupervisorError::from)?,
            ),
        };

        // 5. Preserve the validated child budget.
        // 6. Derive cancellation from the parent agent token. Root agents
        //    that have not been explicitly registered fall back to a token
        //    directly under the session root.
        let child_cancel = self
            .agent_tokens
            .read()
            .expect("agent token lock poisoned")
            .get(&parent.id)
            .map(CancellationToken::child_token)
            .unwrap_or_else(|| self.session_cancel.child_token());

        // 7. Resolve the child's behavior, then construct it and its runner.
        let child_behavior = self
            .child_behavior
            .read()
            .expect("child behavior lock poisoned")
            .clone()
            .resolve(&parent.state.behavior, &spec)?;
        let mut child_agent = Agent::new(
            child_id,
            parent.session_id,
            Some(parent.id),
            parent_depth + 1,
            spec.role.clone().unwrap_or_default(),
            BackendBinding {
                reference: child_backend_reference,
                descriptor: BackendDescriptor {
                    id: BackendId::new(),
                    name: backend.descriptor().name,
                    description: backend.descriptor().description,
                    capabilities: backend.capabilities(),
                },
            },
            capabilities.clone(),
            spec.budget.clone(),
        );
        // M5: apply any execution-param overrides (e.g. a model override
        // requested through `agent.spawn`'s `model` argument) before the
        // child's first run — see `SpawnAgentSpec::execution_params`'s doc
        // comment for why this is a plain state assignment rather than
        // going through `BackendPolicy::Explicit`.
        child_agent.state.execution_params = spec.execution_params.clone();
        child_agent.state.behavior = child_behavior;

        let (task, commands_tx) = AgentTask::new(child_id);
        let (result_tx, result_rx) = oneshot::channel::<AgentResult>();
        let mut runner = AgentRunner::new(
            child_agent,
            task,
            backend,
            tool_registry,
            child_workspace.clone(),
            event_sink.clone(),
            child_cancel.clone(),
            LiveStateTable::default(),
            scheduler,
        )
        .with_supervision(self.clone(), Arc::new(integration_registry.clone()));

        // 8. Run independently, deliver the real terminal command, and clean
        // up concurrent children. AwaitResult is removed by await_child after
        // its result receiver is consumed.
        let agents = Arc::clone(&self.agents);
        let children_of = Arc::clone(&self.children_of);
        let agent_tokens = Arc::clone(&self.agent_tokens);
        let detached = matches!(spec.mode, SpawnMode::Concurrent);
        let completion_commands_tx = parent_commands_tx.clone();
        let (start_tx, start_rx) = oneshot::channel::<()>();
        let join = tokio::spawn(async move {
            // Held for the entire lifetime of this task; released (and the
            // agent-concurrency permit freed) whichever way the task ends.
            let _agent_permit = agent_permit;

            // Registration and parent-state notification must happen before
            // the child is allowed to execute or complete.
            if start_rx.await.is_err() {
                return;
            }
            runner.run().await;

            let outcome = runner.take_final_result();
            let (command, result) = match outcome {
                Some(Ok(result)) => (
                    AgentCommand::ChildCompleted {
                        agent_id: child_id,
                        result: result.clone(),
                    },
                    result,
                ),
                Some(Err(error)) => (
                    AgentCommand::ChildFailed {
                        agent_id: child_id,
                        error,
                    },
                    AgentResult {
                        summary: format!("child {child_id:?} failed"),
                        usage: Default::default(),
                        gate_passed: None,
                    },
                ),
                None => {
                    let result = AgentResult {
                        summary: format!(
                            "child {child_id:?} finished with status {:?}",
                            runner.agent.state.status
                        ),
                        usage: Default::default(),
                        gate_passed: None,
                    };
                    (
                        AgentCommand::ChildCompleted {
                            agent_id: child_id,
                            result: result.clone(),
                        },
                        result,
                    )
                }
            };

            // Both spawn modes re-enter the parent's deterministic state
            // machine exactly once. AwaitResult additionally exposes the
            // terminal value through the one-shot receiver below.
            let _ = completion_commands_tx.send(command).await;
            if detached {
                AgentSupervisor::deregister_shared(&agents, &children_of, &agent_tokens, child_id)
                    .await;
            }
            let _ = result_tx.send(result);
        });

        self.register(
            Some(parent.id),
            child_id,
            parent_depth + 1,
            child_cancel.clone(),
            join,
            commands_tx,
            result_rx,
        )
        .await;

        self.child_capabilities
            .write()
            .await
            .insert(child_id, capabilities);
        self.child_workspaces
            .write()
            .await
            .insert(child_id, child_workspace);
        self.register_agent_token(child_id, child_cancel.clone());

        // Emit `ChildAgentSpawned` (spec §73's "AgentAdded") on the session
        // event sink so frontends observe subagent activity as it happens.
        event_sink.send(AgentEventEnvelope {
            event_id: EventId::new(),
            session_id: parent.session_id,
            agent_id: parent.id,
            parent_agent_id: parent.parent_id,
            run_id: None,
            agent_sequence: 0,
            session_sequence: None,
            timestamp: Timestamp::now(),
            visibility: EventVisibility::User,
            event: AgentEvent::ChildAgentSpawned {
                agent_id: child_id,
                tool_call_id: origin_tool_call_id,
            },
        });

        let _ = start_tx.send(());

        Ok(child_id)
    }

    /// Dispatches SpawnMode::Concurrent versus SpawnMode::AwaitResult.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn_and_drive(
        &self,
        parent: &Agent,
        parent_backend: Arc<dyn ExecutionBackend>,
        integration_registry: &IntegrationRegistry,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        scheduler: Arc<Scheduler>,
        parent_commands_tx: mpsc::Sender<AgentCommand>,
        spec: SpawnAgentSpec,
    ) -> Result<SpawnOutcome, SupervisorError> {
        let mode = spec.mode;
        let child_id = self
            .spawn_child(
                parent,
                parent_backend,
                integration_registry,
                tool_registry,
                workspace,
                event_sink,
                scheduler,
                parent_commands_tx,
                spec,
            )
            .await?;

        match mode {
            SpawnMode::Concurrent => Ok(SpawnOutcome::Detached(child_id)),
            SpawnMode::AwaitResult => Ok(SpawnOutcome::Awaited(Box::new(
                self.await_child(child_id).await?,
            ))),
        }
    }

    pub async fn await_child(&self, child_id: AgentId) -> Result<AgentResult, SupervisorError> {
        let handle = {
            let mut agents = self.agents.write().await;
            agents
                .remove(&child_id)
                .ok_or(SupervisorError::ChildResultLost(child_id))?
        };

        if let Some(parent_id) = handle.parent_id {
            let mut children = self.children_of.write().await;
            if let Some(ids) = children.get_mut(&parent_id) {
                ids.retain(|id| *id != child_id);
            }
        }

        let _ = handle.join.await;
        self.agent_tokens
            .write()
            .expect("agent token lock poisoned")
            .remove(&child_id);
        handle
            .result
            .await
            .map_err(|_| SupervisorError::ChildResultLost(child_id))
    }

    pub async fn child_capabilities(&self, child_id: AgentId) -> Option<AgentCapabilities> {
        self.child_capabilities.read().await.get(&child_id).cloned()
    }

    pub async fn child_workspace(&self, child_id: AgentId) -> Option<Arc<dyn Workspace>> {
        self.child_workspaces.read().await.get(&child_id).cloned()
    }

    pub async fn child_commands(&self, child_id: AgentId) -> Option<mpsc::Sender<AgentCommand>> {
        self.agents
            .read()
            .await
            .get(&child_id)
            .map(|handle| handle.commands.clone())
    }

    /// Cancels one child subtree without affecting its parent or siblings.
    pub async fn cancel_child(&self, child_id: AgentId) -> Result<(), SupervisorError> {
        let agents = self.agents.read().await;
        let handle = agents
            .get(&child_id)
            .ok_or(SupervisorError::ChildResultLost(child_id))?;
        handle.cancel.cancel();
        Ok(())
    }

    /// Purely computes self, descendant, and inclusive usage.
    pub fn inclusive_usage(&self, agent: &Agent) -> harness_core::usage::AgentUsageSummary {
        let self_metrics = agent.usage.self_metrics();
        let children: Vec<harness_core::usage::AgentUsageSummary> =
            agent.usage.child_usage.values().cloned().collect();
        harness_core::usage::compute_agent_usage_summary(self_metrics, &children)
    }

    #[allow(clippy::too_many_arguments)]
    async fn register(
        &self,
        parent_id: Option<AgentId>,
        child_id: AgentId,
        depth: u32,
        cancel: CancellationToken,
        join: JoinHandle<()>,
        commands: mpsc::Sender<AgentCommand>,
        result: oneshot::Receiver<AgentResult>,
    ) {
        let mut agents = self.agents.write().await;
        let mut children = self.children_of.write().await;
        agents.insert(
            child_id,
            AgentHandle {
                parent_id,
                depth,
                cancel,
                join,
                commands,
                result,
            },
        );
        if let Some(parent_id) = parent_id {
            children.entry(parent_id).or_default().push(child_id);
        }
    }

    async fn deregister_shared(
        agents: &RwLock<HashMap<AgentId, AgentHandle>>,
        children_of: &RwLock<HashMap<AgentId, Vec<AgentId>>>,
        agent_tokens: &StdRwLock<HashMap<AgentId, CancellationToken>>,
        child_id: AgentId,
    ) {
        let mut agents = agents.write().await;
        if let Some(handle) = agents.remove(&child_id) {
            if let Some(parent_id) = handle.parent_id {
                let mut children = children_of.write().await;
                if let Some(ids) = children.get_mut(&parent_id) {
                    ids.retain(|id| *id != child_id);
                }
            }
        }
        agent_tokens
            .write()
            .expect("agent token lock poisoned")
            .remove(&child_id);
    }
}

fn reference_to_config(reference: &BackendReference) -> Value {
    serde_json::to_value(reference).unwrap_or_else(|_| serde_json::json!({}))
}

fn validate_child_budget(parent: &AgentBudget, child: &AgentBudget) -> Result<(), SupervisorError> {
    macro_rules! no_looser {
        ($field:ident) => {
            match (parent.$field, child.$field) {
                (Some(parent_limit), Some(child_limit)) if child_limit <= parent_limit => {}
                (Some(_), _) => return Err(SupervisorError::BudgetEscalation(stringify!($field))),
                (None, _) => {}
            }
        };
    }
    no_looser!(max_input_tokens);
    no_looser!(max_output_tokens);
    no_looser!(max_total_tokens);
    no_looser!(max_cost_usd);
    no_looser!(max_requests);
    no_looser!(max_tool_calls);
    no_looser!(max_children);
    no_looser!(max_depth);
    Ok(())
}

#[cfg(test)]
mod tests;
