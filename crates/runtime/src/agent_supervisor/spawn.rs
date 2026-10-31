//! Spawning a child: the eight steps from the parent's request to a running agent.
//!
//! 1. Gate before creating anything: the parent may spawn, the tree has room, the budget fits.
//! 2. Take a slot among the process's active agents.
//! 3. Inherit the parent's backend or create the explicit one.
//! 4. Derive the child's capabilities without escalating the parent's.
//! 5. Resolve the workspace the child works in.
//! 6. Derive its cancellation from the parent agent's token.
//! 7. Resolve its behavior and build the agent and its runner.
//! 8. Run it independently and tell the parent when it finishes.

use std::sync::Arc;

use rusty_agent_core::agent::Agent;
use rusty_agent_core::capabilities::AgentCapabilities;
use rusty_protocol::backend::{BackendBinding, BackendDescriptor, BackendReference};
use rusty_protocol::commands::{AgentCommand, AgentResult};
use rusty_protocol::effects::{BackendPolicy, SpawnAgentSpec, SpawnMode, WorkspacePolicy};
use rusty_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
use rusty_protocol::ids::{AgentId, BackendId, EventId, Timestamp};
use rusty_workspace::{ReadOnlyWorkspace, SnapshotWorkspace, WorktreeWorkspace};
use serde_json::Value;
use tokio::sync::{oneshot, OwnedSemaphorePermit};
use tokio_util::sync::CancellationToken;

use super::budget::validate_child_budget;
use super::host::SpawnHost;
use super::{AgentHandle, AgentSupervisor, SpawnOutcome, SupervisorError};
use crate::agent_runner::{AgentRunner, AgentTask, RunnerServices};
use crate::session_runtime::LiveStateTable;
use crate::traits::{ExecutionBackend, Workspace};

impl AgentSupervisor {
    /// Executes the complete eight-step child spawn flow.
    pub async fn spawn_child(
        &self,
        host: SpawnHost<'_>,
        spec: SpawnAgentSpec,
    ) -> Result<AgentId, SupervisorError> {
        let parent = host.parent;
        self.admit(&host, &spec).await?;
        // Bounds total concurrent agents across the process via
        // `SchedulerConfig::max_active_agents`. Taken before any backend or workspace work so an
        // exhausted cap does not pay for work that would be discarded, and held for the child's
        // whole lifetime.
        let permit = host.scheduler.acquire_agent_permit().await;
        let (backend, reference) = self.child_backend(&host, &spec).await?;
        let capabilities =
            parent
                .capabilities
                .derive_child_agent_capabilities(&spec.tools, None, None)?;
        let child_id = AgentId::new();
        let workspace = child_workspace(&host, &spec, child_id).await?;
        let cancel = self.child_cancellation(parent.id);
        let agent =
            self.child_agent(&host, &spec, child_id, (&backend, reference), &capabilities)?;

        let (task, commands_tx) = AgentTask::new(child_id);
        let runner = AgentRunner::new(
            agent,
            task,
            RunnerServices {
                backend: backend,
                tool_registry: host.tools.clone(),
                workspace: workspace.clone(),
                event_sink: host.events.clone(),
                scheduler: host.scheduler.clone(),
            },
            cancel.clone(),
            LiveStateTable::default(),
        )
        .with_supervision(self.clone(), Arc::new(host.integrations.clone()));

        let detached = matches!(spec.mode, SpawnMode::Concurrent);
        let (start, running) = self.run_child(runner, child_id, permit, &host, detached);
        let handle = AgentHandle {
            parent_id: Some(parent.id),
            depth: parent.state.depth + 1,
            cancel: cancel.clone(),
            join: running.join,
            commands: commands_tx,
            result: running.result,
        };
        self.register(child_id, handle).await;
        self.child_capabilities
            .write()
            .await
            .insert(child_id, capabilities);
        self.child_workspaces
            .write()
            .await
            .insert(child_id, workspace);
        self.register_agent_token(child_id, cancel);
        announce(&host, child_id, spec.origin_tool_call_id);
        // Registration and the parent's notification are done: the child may run.
        let _ = start.send(());
        Ok(child_id)
    }

    /// Dispatches SpawnMode::Concurrent versus SpawnMode::AwaitResult.
    pub async fn spawn_and_drive(
        &self,
        host: SpawnHost<'_>,
        spec: SpawnAgentSpec,
    ) -> Result<SpawnOutcome, SupervisorError> {
        let mode = spec.mode;
        let child_id = self.spawn_child(host, spec).await?;
        match mode {
            SpawnMode::Concurrent => Ok(SpawnOutcome::Detached(child_id)),
            SpawnMode::AwaitResult => Ok(SpawnOutcome::Awaited(Box::new(
                self.await_child(child_id).await?,
            ))),
        }
    }

    /// Step 1. The parent's real nesting depth is used so `max_depth` is enforced across the whole
    /// tree.
    async fn admit(
        &self,
        host: &SpawnHost<'_>,
        spec: &SpawnAgentSpec,
    ) -> Result<(), SupervisorError> {
        let parent = host.parent;
        if !parent.capabilities.can_spawn_agents {
            return Err(SupervisorError::SpawnNotAllowed(parent.id));
        }
        self.max_children_depth_check(parent.id, parent.state.depth, &parent.budget)
            .await?;
        validate_child_budget(&parent.budget, &spec.budget)
    }

    /// Step 3. Inherit the parent backend or instantiate a fresh explicit one.
    async fn child_backend(
        &self,
        host: &SpawnHost<'_>,
        spec: &SpawnAgentSpec,
    ) -> Result<(Arc<dyn ExecutionBackend>, BackendReference), SupervisorError> {
        match &spec.backend {
            BackendPolicy::Inherit => {
                Ok((host.backend.clone(), host.parent.backend.reference.clone()))
            }
            BackendPolicy::Explicit(reference) => {
                let config = reference_to_config(reference);
                let backend = host
                    .integrations
                    .create(&reference.integration.to_string(), config)
                    .await?;
                Ok((backend, reference.clone()))
            }
        }
    }

    /// Step 6. Root agents that were not explicitly registered fall back to a token directly under
    /// the session root.
    fn child_cancellation(&self, parent_id: AgentId) -> CancellationToken {
        self.agent_tokens_ref()
            .get(&parent_id)
            .map(CancellationToken::child_token)
            .unwrap_or_else(|| self.session_cancel.child_token())
    }

    /// Step 7. The child's behavior, then the agent itself.
    fn child_agent(
        &self,
        host: &SpawnHost<'_>,
        spec: &SpawnAgentSpec,
        child_id: AgentId,
        (backend, reference): (&Arc<dyn ExecutionBackend>, BackendReference),
        capabilities: &AgentCapabilities,
    ) -> Result<Agent, SupervisorError> {
        let parent = host.parent;
        let behavior = self
            .child_behavior_resolver()
            .resolve(&parent.state.behavior, spec)?;
        let mut agent = Agent::new(
            child_id,
            parent.session_id,
            Some(parent.id),
            parent.state.depth + 1,
            spec.role.clone().unwrap_or_default(),
            BackendBinding {
                reference,
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
        // Execution-param overrides (e.g. a model override requested through `agent.spawn`'s
        // `model` argument) apply before the child's first run; see
        // `SpawnAgentSpec::execution_params` for why this is a plain state assignment rather than
        // going through `BackendPolicy::Explicit`.
        agent.state.execution_params = spec.execution_params.clone();
        agent.state.behavior = behavior;
        Ok(agent)
    }

    /// Step 8. Runs the child on its own task once `start` is signalled, delivers the real
    /// terminal command to the parent, and cleans up a concurrent child that nobody awaits.
    fn run_child(
        &self,
        mut runner: AgentRunner,
        child_id: AgentId,
        permit: OwnedSemaphorePermit,
        host: &SpawnHost<'_>,
        detached: bool,
    ) -> (oneshot::Sender<()>, RunningChild) {
        let supervisor = self.clone();
        let parent_commands = host.parent_commands.clone();
        let (start, started) = oneshot::channel::<()>();
        let (result_tx, result) = oneshot::channel::<AgentResult>();
        let join = tokio::spawn(async move {
            // Released, freeing the agent-concurrency slot, however the task ends.
            let _permit = permit;
            if started.await.is_err() {
                return;
            }
            runner.run().await;
            let (command, result) = terminal_command(child_id, &mut runner);
            // Both spawn modes re-enter the parent's deterministic state machine exactly once.
            // AwaitResult additionally exposes the terminal value through the receiver.
            let _ = parent_commands.send(command).await;
            if detached {
                supervisor.deregister(child_id).await;
            }
            let _ = result_tx.send(result);
        });
        (start, RunningChild { join, result })
    }
}

/// The child's task and the channel its result arrives on.
struct RunningChild {
    join: tokio::task::JoinHandle<()>,
    result: oneshot::Receiver<AgentResult>,
}

/// Step 5. The workspace policy the spec asks for.
async fn child_workspace(
    host: &SpawnHost<'_>,
    spec: &SpawnAgentSpec,
    child_id: AgentId,
) -> Result<Arc<dyn Workspace>, SupervisorError> {
    let workspace = host.workspace.clone();
    Ok(match &spec.workspace {
        WorkspacePolicy::Inherit => workspace,
        WorkspacePolicy::ReadOnly => Arc::new(ReadOnlyWorkspace::new(workspace)),
        WorkspacePolicy::Snapshot => Arc::new(SnapshotWorkspace::create(workspace.root()).await?),
        WorkspacePolicy::NewWorktree => {
            Arc::new(WorktreeWorkspace::create(workspace.root(), &child_id.to_string()).await?)
        }
    })
}

/// What the parent is told when the child ends, and the result an awaiter receives.
fn terminal_command(child_id: AgentId, runner: &mut AgentRunner) -> (AgentCommand, AgentResult) {
    match runner.take_final_result() {
        Some(Ok(result)) => (
            AgentCommand::ChildCompleted {
                agent_id: child_id,
                result: result.clone(),
            },
            result,
        ),
        Some(Err(error)) => {
            let result = placeholder_result(format!("child {child_id:?} failed"));
            (
                AgentCommand::ChildFailed {
                    agent_id: child_id,
                    error,
                },
                result,
            )
        }
        None => {
            let status = runner.agent.state.status;
            let result = placeholder_result(format!(
                "child {child_id:?} finished with status {status:?}"
            ));
            (
                AgentCommand::ChildCompleted {
                    agent_id: child_id,
                    result: result.clone(),
                },
                result,
            )
        }
    }
}

fn placeholder_result(summary: String) -> AgentResult {
    AgentResult {
        summary,
        usage: Default::default(),
        gate_passed: None,
    }
}

/// Emits `ChildAgentSpawned` on the session event sink so frontends see subagent activity as it
/// happens.
fn announce(
    host: &SpawnHost<'_>,
    child_id: AgentId,
    tool_call_id: Option<rusty_protocol::ids::ToolCallId>,
) {
    let parent = host.parent;
    host.events.send(AgentEventEnvelope {
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
            tool_call_id,
        },
    });
}

fn reference_to_config(reference: &BackendReference) -> Value {
    serde_json::to_value(reference).unwrap_or_else(|_| serde_json::json!({}))
}
