//! Hierarchical supervision and child-agent spawning for one session.

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
    Awaited(AgentResult),
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
        }
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
        // 1. Gate before creating a child task. The parent's real nesting depth
        //    is used so `max_depth` is enforced across the whole tree.
        let parent_depth = parent.state.depth;
        if !parent.capabilities.can_spawn_agents {
            return Err(SupervisorError::SpawnNotAllowed(parent.id));
        }
        self.max_children_depth_check(parent.id, parent_depth, &parent.budget)
            .await?;
        validate_child_budget(&parent.budget, &spec.budget)?;

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

        // 7. Construct the child and its runner.
        let child_agent = Agent::new(
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
                    },
                ),
                None => {
                    let result = AgentResult {
                        summary: format!(
                            "child {child_id:?} finished with status {:?}",
                            runner.agent.state.status
                        ),
                        usage: Default::default(),
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
            event: AgentEvent::ChildAgentSpawned { agent_id: child_id },
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
            SpawnMode::AwaitResult => Ok(SpawnOutcome::Awaited(self.await_child(child_id).await?)),
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
        let records = agent.usage.records.clone();
        let children: Vec<harness_core::usage::AgentUsageSummary> =
            agent.usage.child_usage.values().cloned().collect();
        harness_core::usage::compute_agent_usage_summary(&records, &children)
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
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Duration;

    use harness_core::capabilities::WorkspaceCapabilities;
    use harness_protocol::backend::{
        BackendCapabilities, BackendDescriptor, BackendReference, ExecutionEvent, ExecutionResult,
    };
    use harness_protocol::commands::{AgentResult, AgentStatus, UserInput};
    use harness_protocol::effects::ToolInheritance;
    use harness_protocol::events::{AgentEvent, AgentEventEnvelope};
    use harness_protocol::ids::{ConfigurationId, IntegrationId, RequestId, ToolCallId, ToolId};
    use harness_protocol::tools::{
        AgentToolset, PermissionMode, ToolCall, ToolCapability, ToolDescriptor, ToolPolicy,
    };
    use harness_protocol::usage::{
        AgentUsageMetrics, AgentUsageSummary, Cost, ModelUsage, UsageRecord, UsageValue,
    };

    use crate::scheduler::SchedulerConfig;
    use crate::testing::{FakeBackend, FakeToolRegistry};
    use crate::traits::EventSink;
    use crate::workspace::FakeWorkspace;

    /// A thread-safe event sink that records every envelope it receives.
    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<AgentEventEnvelope>>,
    }
    impl EventSink for RecordingSink {
        fn send(&self, envelope: AgentEventEnvelope) {
            self.events.lock().unwrap().push(envelope);
        }
    }

    /// Builds a parent agent capable of spawning children (empty toolset,
    /// default budget so the `max_children`/`max_depth` gate is open).
    fn parent(session_id: SessionId) -> Agent {
        Agent::new(
            AgentId::new(),
            session_id,
            None,
            0,
            "parent".into(),
            BackendBinding {
                reference: BackendReference {
                    integration: IntegrationId::new(),
                    configuration: ConfigurationId::new(),
                    model: None,
                },
                descriptor: BackendDescriptor {
                    id: BackendId::new(),
                    name: "fake".into(),
                    description: "fake".into(),
                    capabilities: BackendCapabilities::default(),
                },
            },
            AgentCapabilities {
                tools: AgentToolset {
                    tools: HashMap::new(),
                },
                can_spawn_agents: true,
                max_child_depth: Some(5),
                workspace: WorkspaceCapabilities {
                    can_read: true,
                    can_write: true,
                    can_search: true,
                },
                backend: BackendCapabilities::default(),
            },
            AgentBudget::default(),
        )
    }

    /// A scripted backend that streams a single text delta and completes.
    fn scripted_backend(text: &str) -> FakeBackend {
        let request_id = RequestId::new();
        FakeBackend::new()
            .with_events(vec![ExecutionEvent::TextDelta {
                request_id,
                delta: text.to_string(),
            }])
            .with_result(ExecutionResult {
                request_id,
                usage: Default::default(),
                cost: Default::default(),
                finish_reason: "end_turn".into(),
            })
    }

    #[test]
    fn child_budget_cannot_remove_or_raise_parent_limits() {
        let parent = AgentBudget {
            max_total_tokens: Some(100),
            max_cost_usd: Some(rust_decimal::Decimal::new(500, 2)),
            max_children: Some(2),
            max_depth: Some(3),
            ..Default::default()
        };

        let unlimited_tokens = AgentBudget {
            max_cost_usd: parent.max_cost_usd,
            max_children: parent.max_children,
            max_depth: parent.max_depth,
            ..Default::default()
        };
        assert!(matches!(
            validate_child_budget(&parent, &unlimited_tokens),
            Err(SupervisorError::BudgetEscalation("max_total_tokens"))
        ));

        let tighter = AgentBudget {
            max_total_tokens: Some(80),
            max_cost_usd: Some(rust_decimal::Decimal::new(400, 2)),
            max_children: Some(1),
            max_depth: Some(2),
            ..Default::default()
        };
        assert!(validate_child_budget(&parent, &tighter).is_ok());
    }

    /// Polls `sink` (with timeout) until a recorded envelope matches `predicate`.
    async fn sink_eventually(
        sink: &RecordingSink,
        predicate: impl Fn(&AgentEvent) -> bool,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            {
                let events = sink.events.lock().unwrap();
                if events.iter().any(|env| predicate(&env.event)) {
                    return true;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn max_children_gate_rejects_third_child() {
        let supervisor = AgentSupervisor::new(SessionId::new(), SessionCancellation::new());
        let parent_id = AgentId::new();
        let budget = AgentBudget {
            max_children: Some(2),
            ..Default::default()
        };

        for _ in 0..2 {
            let child_id = AgentId::new();
            let cancel = supervisor.session_cancel.child_token();
            let join = tokio::spawn(async {});
            let (commands, _) = mpsc::channel(1);
            let (_, result) = oneshot::channel();
            supervisor
                .register(Some(parent_id), child_id, 1, cancel, join, commands, result)
                .await;
        }

        let error = supervisor
            .max_children_depth_check(parent_id, 0, &budget)
            .await
            .expect_err("third child must be rejected");
        assert!(matches!(
            error,
            SupervisorError::Budget(BudgetError::TooManyChildren { .. })
        ));
    }

    /// Integration test (Task 6.2): spawning a child with
    /// `ToolInheritance::Subset([fs.read])` from a parent that also holds a
    /// non-delegatable `shell.exec` yields a child whose capability set
    /// contains exactly `[fs.read]` (non-escalation invariant).
    #[tokio::test]
    async fn spawn_child_inherits_only_delegatable_tools() {
        let session_id = SessionId::new();
        let supervisor = AgentSupervisor::new(session_id, SessionCancellation::new());

        // Parent toolset: fs.read (delegatable) + shell.exec (not delegatable).
        let fs_read_id = ToolId::new();
        let shell_exec_id = ToolId::new();
        let mut tools = HashMap::new();
        tools.insert(
            fs_read_id,
            ToolCapability {
                descriptor: ToolDescriptor {
                    id: fs_read_id,
                    name: "fs.read".into(),
                    description: "Read files".into(),
                    input_schema: serde_json::json!({}),
                },
                policy: ToolPolicy {
                    permission: PermissionMode::Allow,
                    enabled: true,
                },
                delegatable: true,
            },
        );
        tools.insert(
            shell_exec_id,
            ToolCapability {
                descriptor: ToolDescriptor {
                    id: shell_exec_id,
                    name: "shell.exec".into(),
                    description: "Execute shell commands".into(),
                    input_schema: serde_json::json!({}),
                },
                policy: ToolPolicy {
                    permission: PermissionMode::Allow,
                    enabled: true,
                },
                delegatable: false,
            },
        );

        let parent = Agent::new(
            AgentId::new(),
            session_id,
            None,
            0,
            "system".into(),
            BackendBinding {
                reference: BackendReference {
                    integration: IntegrationId::new(),
                    configuration: ConfigurationId::new(),
                    model: None,
                },
                descriptor: BackendDescriptor {
                    id: BackendId::new(),
                    name: "fake".into(),
                    description: "fake".into(),
                    capabilities: BackendCapabilities::default(),
                },
            },
            AgentCapabilities {
                tools: AgentToolset { tools },
                can_spawn_agents: true,
                max_child_depth: Some(5),
                workspace: WorkspaceCapabilities {
                    can_read: true,
                    can_write: false,
                    can_search: false,
                },
                backend: BackendCapabilities::default(),
            },
            AgentBudget::default(),
        );

        let parent_backend: Arc<dyn ExecutionBackend> = Arc::new(FakeBackend::new());
        let tool_registry: Arc<dyn ToolRegistry> = Arc::new(FakeToolRegistry::new());
        let workspace: Arc<dyn Workspace> = Arc::new(FakeWorkspace::new());
        let event_sink: Arc<dyn EventSink> = Arc::new(RecordingSink::default());
        let scheduler: Arc<Scheduler> = Arc::new(Scheduler::new(SchedulerConfig::default()));
        let integration_registry = IntegrationRegistry::new();
        let (parent_commands_tx, _parent_commands_rx) = mpsc::channel(64);

        let spec = SpawnAgentSpec {
            role: Some("child".into()),
            backend: BackendPolicy::Inherit,
            tools: ToolInheritance::Subset(vec![fs_read_id]),
            workspace: WorkspacePolicy::Inherit,
            budget: AgentBudget::default(),
            mode: SpawnMode::Concurrent,
        };

        let child_id = supervisor
            .spawn_child(
                &parent,
                parent_backend,
                &integration_registry,
                tool_registry,
                workspace,
                event_sink,
                scheduler,
                parent_commands_tx,
                spec,
            )
            .await
            .expect("spawn child should succeed");

        let child_caps = supervisor
            .child_capabilities(child_id)
            .await
            .expect("child capabilities should be retained by the supervisor");

        assert_eq!(
            child_caps.tools.tools.len(),
            1,
            "child must inherit exactly one tool (fs.read), got {:?}",
            child_caps.tools.tools.keys().collect::<Vec<_>>()
        );
        assert!(
            child_caps.tools.tools.contains_key(&fs_read_id),
            "fs.read must be inherited by the child"
        );
        assert!(
            !child_caps.tools.tools.contains_key(&shell_exec_id),
            "shell.exec must not be inherited by the child"
        );
    }

    /// Task 6.4 — `SpawnMode::AwaitResult`: spawning two children blocks the
    /// calling task until **both** `await_child` calls return. Observed via a
    /// channel that is only signalled after each `spawn_and_drive` future
    /// resolves: before the session is cancelled neither signal has fired, and
    /// after cancellation both children terminate and both `Awaited` outcomes
    /// arrive.
    #[tokio::test]
    async fn spawn_and_drive_await_result_blocks_until_children_finish() {
        let session_id = SessionId::new();
        let supervisor = Arc::new(AgentSupervisor::new(session_id, SessionCancellation::new()));
        let parent = parent(session_id);
        let parent_backend: Arc<dyn ExecutionBackend> = Arc::new(FakeBackend::new());
        let tool_registry: Arc<dyn ToolRegistry> = Arc::new(FakeToolRegistry::new());
        let workspace: Arc<dyn Workspace> = Arc::new(FakeWorkspace::new());
        let event_sink: Arc<dyn EventSink> = Arc::new(RecordingSink::default());
        let scheduler: Arc<Scheduler> = Arc::new(Scheduler::new(SchedulerConfig::default()));
        let (parent_commands_tx, _parent_commands_rx) = mpsc::channel(64);

        // Signalled once each awaited spawn returns.
        let (done_tx, mut done_rx) = mpsc::channel::<SpawnOutcome>(2);

        #[allow(clippy::too_many_arguments)]
        async fn drive_and_await(
            supervisor: Arc<AgentSupervisor>,
            parent: Agent,
            parent_backend: Arc<dyn ExecutionBackend>,
            tool_registry: Arc<dyn ToolRegistry>,
            workspace: Arc<dyn Workspace>,
            event_sink: Arc<dyn EventSink>,
            scheduler: Arc<Scheduler>,
            parent_commands_tx: mpsc::Sender<AgentCommand>,
            done_tx: mpsc::Sender<SpawnOutcome>,
        ) -> SpawnOutcome {
            let integration_registry = IntegrationRegistry::new();
            let outcome = supervisor
                .spawn_and_drive(
                    &parent,
                    parent_backend,
                    &integration_registry,
                    tool_registry,
                    workspace,
                    event_sink,
                    scheduler,
                    parent_commands_tx,
                    SpawnAgentSpec {
                        role: Some("child".into()),
                        backend: BackendPolicy::Inherit,
                        tools: ToolInheritance::InheritAll,
                        workspace: WorkspacePolicy::Inherit,
                        budget: AgentBudget::default(),
                        mode: SpawnMode::AwaitResult,
                    },
                )
                .await
                .expect("spawn_and_drive should succeed");
            let _ = done_tx.send(outcome.clone()).await;
            outcome
        }

        let spawn_child_a = tokio::spawn(drive_and_await(
            Arc::clone(&supervisor),
            parent.clone(),
            Arc::clone(&parent_backend),
            Arc::clone(&tool_registry),
            Arc::clone(&workspace),
            Arc::clone(&event_sink),
            Arc::clone(&scheduler),
            parent_commands_tx.clone(),
            done_tx.clone(),
        ));
        let spawn_child_b = tokio::spawn(drive_and_await(
            Arc::clone(&supervisor),
            parent.clone(),
            Arc::clone(&parent_backend),
            Arc::clone(&tool_registry),
            Arc::clone(&workspace),
            Arc::clone(&event_sink),
            Arc::clone(&scheduler),
            parent_commands_tx.clone(),
            done_tx.clone(),
        ));

        // Give both `spawn_and_drive` futures time to reach `await_child`.
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(
            done_rx.try_recv().is_err(),
            "parent effect processing must pause until both awaited children complete"
        );

        // Cancel the session so both children's background runners terminate
        // and deliver their final results over the one-shot channels.
        supervisor.session_cancel.cancel();

        let outcome_a = tokio::time::timeout(Duration::from_secs(2), spawn_child_a)
            .await
            .expect("child A should complete promptly")
            .expect("child A task should not panic");
        let outcome_b = tokio::time::timeout(Duration::from_secs(2), spawn_child_b)
            .await
            .expect("child B should complete promptly")
            .expect("child B task should not panic");

        assert!(matches!(outcome_a, SpawnOutcome::Awaited(_)));
        assert!(matches!(outcome_b, SpawnOutcome::Awaited(_)));

        assert!(done_rx.try_recv().is_ok(), "child A signalled completion");
        assert!(done_rx.try_recv().is_ok(), "child B signalled completion");
    }

    /// Task 6.4 — `SpawnMode::Concurrent`: `spawn_and_drive` returns
    /// `Detached` immediately while the child continues running in the
    /// background; once it finishes, the child is deregistered by its
    /// completion task.
    #[tokio::test]
    async fn spawn_and_drive_concurrent_returns_detached_immediately() {
        let session_id = SessionId::new();
        let supervisor = AgentSupervisor::new(session_id, SessionCancellation::new());
        let parent = parent(session_id);

        let parent_backend: Arc<dyn ExecutionBackend> = Arc::new(scripted_backend("child-work"));
        let tool_registry: Arc<dyn ToolRegistry> = Arc::new(FakeToolRegistry::new());
        let workspace: Arc<dyn Workspace> = Arc::new(FakeWorkspace::new());
        let event_sink: Arc<dyn EventSink> = Arc::new(RecordingSink::default());
        let scheduler: Arc<Scheduler> = Arc::new(Scheduler::new(SchedulerConfig::default()));
        let integration_registry = IntegrationRegistry::new();
        let (parent_commands_tx, _parent_commands_rx) = mpsc::channel(64);

        let spec = SpawnAgentSpec {
            role: Some("child".into()),
            backend: BackendPolicy::Inherit,
            tools: ToolInheritance::InheritAll,
            workspace: WorkspacePolicy::Inherit,
            budget: AgentBudget::default(),
            mode: SpawnMode::Concurrent,
        };

        // This must return immediately (not block on the child).
        let outcome = supervisor
            .spawn_and_drive(
                &parent,
                parent_backend,
                &integration_registry,
                tool_registry,
                workspace,
                event_sink,
                scheduler,
                parent_commands_tx,
                spec,
            )
            .await
            .expect("concurrent spawn should succeed");

        let child_id = match outcome {
            SpawnOutcome::Detached(id) => id,
            other => panic!("expected Detached, got {other:?}"),
        };

        // The child is still tracked: spawn_and_drive did not wait for it.
        let child_commands = supervisor
            .child_commands(child_id)
            .await
            .expect("detached child should still be running in the background");

        // Drive the detached child to actually do work in the background.
        child_commands
            .send(AgentCommand::StartRun {
                input: UserInput {
                    text: "go".into(),
                    attachments: vec![],
                },
            })
            .await
            .expect("send StartRun to detached child");

        // The child should complete on its own in the background; its spawned
        // completion task deregisters it from the registry once it finishes.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let tracked = { supervisor.agents.read().await.contains_key(&child_id) };
            if !tracked {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "detached child should complete and be deregistered in the background"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Task 6.6 — spawning a child emits `AgentEvent::ChildAgentSpawned` on
    /// the session event sink (spec §73's "AgentAdded A1").
    #[tokio::test]
    async fn spawn_child_emits_child_agent_spawned_event() {
        let session_id = SessionId::new();
        let supervisor = AgentSupervisor::new(session_id, SessionCancellation::new());
        let parent = parent(session_id);

        let parent_backend: Arc<dyn ExecutionBackend> = Arc::new(FakeBackend::new());
        let tool_registry: Arc<dyn ToolRegistry> = Arc::new(FakeToolRegistry::new());
        let workspace: Arc<dyn Workspace> = Arc::new(FakeWorkspace::new());
        let recording = Arc::new(RecordingSink::default());
        let event_sink: Arc<dyn EventSink> = recording.clone();
        let scheduler: Arc<Scheduler> = Arc::new(Scheduler::new(SchedulerConfig::default()));
        let integration_registry = IntegrationRegistry::new();
        let (parent_commands_tx, _parent_commands_rx) = mpsc::channel(64);

        let spec = SpawnAgentSpec {
            role: Some("A1".into()),
            backend: BackendPolicy::Inherit,
            tools: ToolInheritance::InheritAll,
            workspace: WorkspacePolicy::Inherit,
            budget: AgentBudget::default(),
            mode: SpawnMode::Concurrent,
        };

        let child_id = supervisor
            .spawn_child(
                &parent,
                parent_backend,
                &integration_registry,
                tool_registry,
                workspace,
                event_sink,
                scheduler,
                parent_commands_tx,
                spec,
            )
            .await
            .expect("root should spawn child A1");

        let events = recording.events.lock().unwrap();
        assert!(
            events.iter().any(|env| matches!(
                &env.event,
                AgentEvent::ChildAgentSpawned { agent_id } if *agent_id == child_id
            )),
            "ChildAgentSpawned should be emitted for the new child"
        );
    }

    /// Task 6.6 — spec §73 ordered stream: when a root spawns a child that
    /// completes, the session event stream shows `AgentAdded A1 → status
    /// updates → tool activity → A1 text → A1 completed`, and the parent
    /// receives the real `ChildCompleted` command (usage updated).
    #[tokio::test]
    async fn root_spawns_child_event_stream_matches_section_73_order() {
        let session_id = SessionId::new();
        let supervisor = AgentSupervisor::new(session_id, SessionCancellation::new());

        let parent = parent(session_id);

        // Child backend: tool activity, then streamed text, then completion.
        let request_id = RequestId::new();
        let child_backend: Arc<dyn ExecutionBackend> = Arc::new(
            FakeBackend::new()
                .with_events(vec![
                    ExecutionEvent::ToolCallRequested {
                        request_id,
                        call: ToolCall {
                            id: ToolCallId::new(),
                            name: "fs.read".into(),
                            arguments: serde_json::json!({}),
                        },
                    },
                    ExecutionEvent::TextDelta {
                        request_id,
                        delta: "A1 text".into(),
                    },
                ])
                .with_result(ExecutionResult {
                    request_id,
                    usage: Default::default(),
                    cost: Default::default(),
                    finish_reason: "end_turn".into(),
                }),
        );

        let tool_registry: Arc<dyn ToolRegistry> = Arc::new(FakeToolRegistry::new());
        let workspace: Arc<dyn Workspace> = Arc::new(FakeWorkspace::new());
        let recording = Arc::new(RecordingSink::default());
        let event_sink: Arc<dyn EventSink> = recording.clone();
        let scheduler: Arc<Scheduler> = Arc::new(Scheduler::new(SchedulerConfig::default()));
        let integration_registry = IntegrationRegistry::new();
        let (parent_commands_tx, mut parent_commands_rx) = mpsc::channel::<AgentCommand>(64);

        let spec = SpawnAgentSpec {
            role: Some("A1".into()),
            backend: BackendPolicy::Inherit,
            tools: ToolInheritance::InheritAll,
            workspace: WorkspacePolicy::Inherit,
            budget: AgentBudget::default(),
            mode: SpawnMode::Concurrent,
        };

        let outcome = supervisor
            .spawn_and_drive(
                &parent,
                child_backend,
                &integration_registry,
                tool_registry,
                workspace,
                event_sink,
                scheduler,
                parent_commands_tx,
                spec,
            )
            .await
            .expect("root should spawn child A1");

        let child_id = match outcome {
            SpawnOutcome::Detached(id) => id,
            other => panic!("expected Detached, got {other:?}"),
        };

        // Drive the child so its run actually executes its scripted backend.
        let commands = supervisor
            .child_commands(child_id)
            .await
            .expect("spawned child must be tracked");
        commands
            .send(AgentCommand::StartRun {
                input: UserInput {
                    text: "go".into(),
                    attachments: vec![],
                },
            })
            .await
            .expect("send StartRun to child A1");

        // Wait for the child run to complete (its `Completed` event appears in
        // the event stream).
        assert!(
            sink_eventually(&recording, |e| matches!(e, AgentEvent::Completed { .. })).await,
            "child A1 should complete within the polling window"
        );

        // Retrieve the real `ChildCompleted` command the child's completion
        // task delivered into the parent's mailbox.
        let command = tokio::time::timeout(Duration::from_secs(2), parent_commands_rx.recv())
            .await
            .expect("parent should receive a child command promptly")
            .expect("child command channel should not close");
        assert!(
            matches!(&command, AgentCommand::ChildCompleted { agent_id, .. } if *agent_id == child_id),
            "parent should receive ChildCompleted for A1, got {command:?}"
        );

        // The parent applies the child's completion, rolling its usage into
        // the parent's ledger (usage updated).
        let mut root = parent.clone();
        root.apply(command);
        assert!(
            root.usage.child_usage.contains_key(&child_id),
            "A usage should be updated into the parent's ledger after A1 completes"
        );

        // Verify the exact ordered event stream:
        //   agent added → status updates → tool activity → A1 text → A1 completed.
        let events: Vec<AgentEvent> = recording
            .events
            .lock()
            .unwrap()
            .iter()
            .map(|env| env.event.clone())
            .collect();
        let pos = |f: &dyn Fn(&AgentEvent) -> bool| events.iter().position(f);

        let agent_added = pos(&|e| matches!(e, AgentEvent::ChildAgentSpawned { .. }))
            .expect("agent added should be present");
        let tool_requested = pos(&|e| matches!(e, AgentEvent::ToolCallRequested { .. }))
            .expect("tool activity should be present");
        let text_delta = pos(&|e| matches!(e, AgentEvent::AssistantTextDelta { .. }))
            .expect("A1 streamed text should be present");
        let a1_completed = pos(&|e| matches!(e, AgentEvent::Completed { .. }))
            .expect("A1 completed should be present");

        // Status updates (StateChanged) must appear after the agent is added
        // and before tool activity.
        assert!(
            events[agent_added + 1..tool_requested]
                .iter()
                .any(|e| matches!(e, AgentEvent::StateChanged { .. })),
            "status updates should follow the agent being added and precede tool activity"
        );

        assert!(
            agent_added < tool_requested,
            "agent added must precede tool activity"
        );
        assert!(
            tool_requested < text_delta,
            "tool activity must precede A1 text"
        );
        assert!(
            text_delta < a1_completed,
            "A1 text must precede A1 completion"
        );
    }

    /// Task 6.7 — `inclusive_usage` aggregates the parent's own records with
    /// the rolled-up summaries its children reported via the `ChildCompleted`
    /// flow: `inclusive_usage == self_usage + Σ children's self_usage`,
    /// numerically verified, without mutating the parent's own records.
    #[test]
    fn inclusive_usage_sums_parent_self_and_children_self_usage() {
        let session_id = SessionId::new();
        let supervisor = AgentSupervisor::new(session_id, SessionCancellation::new());
        let mut root = parent(session_id);

        // Parent self usage: two records of 100 total tokens each = 200.
        root.usage.records.push(UsageRecord {
            model_usage: ModelUsage {
                total_tokens: UsageValue::new(Some(100)),
                ..Default::default()
            },
            cost: Cost::default(),
            tool_usage: None,
        });
        root.usage.records.push(UsageRecord {
            model_usage: ModelUsage {
                total_tokens: UsageValue::new(Some(100)),
                ..Default::default()
            },
            cost: Cost::default(),
            tool_usage: None,
        });

        let child_a = AgentId::new();
        let child_b = AgentId::new();
        root.state.children = vec![child_a, child_b];
        root.state.status = AgentStatus::WaitingForChildren;

        // Child A reports 50 total tokens of self usage.
        let usage_a = AgentUsageSummary {
            self_usage: AgentUsageMetrics {
                total_tokens: UsageValue::new(Some(50)),
                ..Default::default()
            },
            inclusive_usage: AgentUsageMetrics {
                total_tokens: UsageValue::new(Some(50)),
                ..Default::default()
            },
            ..Default::default()
        };
        // Child B reports 30 total tokens of self usage.
        let usage_b = AgentUsageSummary {
            self_usage: AgentUsageMetrics {
                total_tokens: UsageValue::new(Some(30)),
                ..Default::default()
            },
            inclusive_usage: AgentUsageMetrics {
                total_tokens: UsageValue::new(Some(30)),
                ..Default::default()
            },
            ..Default::default()
        };

        root.apply(AgentCommand::ChildCompleted {
            agent_id: child_a,
            result: AgentResult {
                summary: "a".into(),
                usage: usage_a.clone(),
            },
        });
        root.apply(AgentCommand::ChildCompleted {
            agent_id: child_b,
            result: AgentResult {
                summary: "b".into(),
                usage: usage_b.clone(),
            },
        });

        let summary = supervisor.inclusive_usage(&root);

        // self_usage = parent's own records (200).
        assert_eq!(
            summary.self_usage.total_tokens.value(),
            Some(200),
            "parent self usage should aggregate its own records"
        );
        // descendant_usage = sum of children's inclusive usage (50 + 30 = 80).
        assert_eq!(
            summary.descendant_usage.total_tokens.value(),
            Some(80),
            "descendant usage should sum the children's rolled-up usage"
        );
        // inclusive_usage = parent self usage + sum of children's self usage
        // (200 + 50 + 30 = 280).
        assert_eq!(
            summary.inclusive_usage.total_tokens.value(),
            Some(280),
            "inclusive usage should equal parent self usage plus children's self usage"
        );
    }

    #[test]
    fn inclusive_usage_is_a_pure_aggregate() {
        let session = SessionId::new();
        let supervisor = AgentSupervisor::new(session, SessionCancellation::new());
        let root = parent(session);
        let before = root.usage.records.len();
        let summary = supervisor.inclusive_usage(&root);
        assert_eq!(summary.self_usage.total_tokens.value(), None);
        assert_eq!(root.usage.records.len(), before);
    }
}
