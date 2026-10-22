//! Child agents: the supervisor a runner spawns them through, and the
//! results the `agent.spawn` tool reports back to the calling model.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::mpsc;

use harness_core::agent::Agent;
use harness_protocol::commands::{AgentCommand, AgentResult, UserInput};
use harness_protocol::effects::SpawnAgentSpec;
use harness_protocol::ids::{AgentId, ToolCallId};
use harness_protocol::tools::ToolResult;

use crate::agent_supervisor::{AgentSupervisor, SupervisorError};
use crate::integration::IntegrationRegistry;
use crate::scheduler::Scheduler;
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

/// Session-scoped services required to interpret child-agent effects.
#[derive(Clone)]
pub struct AgentSupervision {
    pub(super) supervisor: Arc<dyn SupervisorControl>,
    pub integrations: Arc<IntegrationRegistry>,
}

impl AgentSupervision {
    pub(super) fn new(supervisor: AgentSupervisor, integrations: Arc<IntegrationRegistry>) -> Self {
        Self {
            supervisor: Arc::new(supervisor),
            integrations,
        }
    }
}

#[async_trait]
pub(super) trait SupervisorControl: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    async fn spawn_child(
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
    ) -> Result<AgentId, SupervisorError>;

    async fn await_child(&self, child_id: AgentId) -> Result<AgentResult, SupervisorError>;
    async fn cancel_child(&self, child_id: AgentId) -> Result<(), SupervisorError>;
    /// M5: fetches the command channel for a just-spawned child so its
    /// initial task (`SpawnAgentSpec::task`) can be delivered as a
    /// `StartRun`. `None` if the child isn't registered (already finished,
    /// or the id is stale).
    async fn child_commands(&self, child_id: AgentId) -> Option<mpsc::Sender<AgentCommand>>;
}

/// Outcome of a spawn attempt, returned so callers other than the plain
/// `AgentEffect::SpawnAgent` path (namely the `agent.spawn` tool — see
/// `AgentRunner::execute_tool`) can build a meaningful result of their own
/// without duplicating any of the supervisor-enforced spawn logic.
///
/// `awaited` is best-effort observability only: the *authoritative*
/// state-machine update always happens independently, via the child's own
/// runner sending `AgentCommand::ChildCompleted`/`ChildFailed` directly to
/// this agent's mailbox when it finishes (see
/// `AgentSupervisor::spawn_child`) — this field exists only so a
/// tool-triggered `AwaitResult` spawn can report what happened back to the
/// calling model in its `ToolResult`, not to drive any core state.
///
/// Distinct from `agent_supervisor::SpawnOutcome` (which distinguishes
/// `Detached`/`Awaited` for `spawn_and_drive`'s own callers) — this type is
/// specifically the `agent_runner`-level view used by the tool-interception
/// path in `execute_tool`.
pub(crate) struct ToolSpawnOutcome {
    pub child_id: AgentId,
    pub awaited: Option<Result<AgentResult, String>>,
}

#[async_trait]
impl SupervisorControl for AgentSupervisor {
    async fn spawn_child(
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
        AgentSupervisor::spawn_child(
            self,
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
        .await
    }

    async fn await_child(&self, child_id: AgentId) -> Result<AgentResult, SupervisorError> {
        AgentSupervisor::await_child(self, child_id).await
    }

    async fn cancel_child(&self, child_id: AgentId) -> Result<(), SupervisorError> {
        AgentSupervisor::cancel_child(self, child_id).await
    }

    async fn child_commands(&self, child_id: AgentId) -> Option<mpsc::Sender<AgentCommand>> {
        AgentSupervisor::child_commands(self, child_id).await
    }
}

/// M5: kick off a just-spawned child's initial task.
pub(super) async fn start_child_task(
    supervisor: &dyn SupervisorControl,
    child_id: AgentId,
    task: String,
) {
    let Some(child_tx) = supervisor.child_commands(child_id).await else {
        tracing::warn!(
            ?child_id,
            "spawned child has no command channel; its task was not started"
        );
        return;
    };
    let start = AgentCommand::StartRun {
        input: UserInput {
            text: task,
            attachments: vec![],
        },
    };
    if child_tx.send(start).await.is_err() {
        tracing::warn!(
            ?child_id,
            "spawned child stopped before its task was started"
        );
    }
}

/// The `agent.spawn` result for a child that was spawned, whatever became
/// of it after.
pub(super) fn spawned_child_result(
    call_id: ToolCallId,
    outcome: &ToolSpawnOutcome,
    warnings: &[String],
) -> ToolResult {
    let child_agent_id = outcome.child_id.to_string();
    let (is_error, mut output) = match &outcome.awaited {
        None => (
            false,
            json!({
                "child_agent_id": child_agent_id,
                "status": "spawned",
                "note": "running concurrently; its own completion will appear as ChildAgentCompleted",
            }),
        ),
        Some(Ok(result)) => {
            let mut output = json!({
                "child_agent_id": child_agent_id,
                "status": "completed",
                "summary": result.summary,
            });
            // Tell the parent when the child finished without passing its
            // completion gate.
            if let Some(passed) = result.gate_passed {
                output["gate_passed"] = json!(passed);
            }
            (false, output)
        }
        Some(Err(message)) => (
            true,
            json!({
                "child_agent_id": child_agent_id,
                "status": "failed",
                "error": message,
            }),
        ),
    };
    if !warnings.is_empty() {
        output["warnings"] = json!(warnings);
    }
    ToolResult {
        call_id,
        output,
        is_error,
    }
}

/// The `agent.spawn` result when no child was spawned.
pub(super) fn spawn_refused_result(call_id: ToolCallId, message: String) -> ToolResult {
    ToolResult {
        call_id,
        output: json!({ "error": message }),
        is_error: true,
    }
}
