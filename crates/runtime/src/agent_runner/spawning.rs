//! Child agents the runner starts, waits for and cancels on the agent's behalf.

use rusty_protocol::commands::{AgentCommand, AgentError, AgentResult};
use rusty_protocol::effects::{SpawnAgentSpec, SpawnMode};
use rusty_protocol::ids::{AgentId, ToolCallId};

use super::children::{
    spawn_refused_result, spawned_child_result, start_child_task, ToolSpawnOutcome,
};

use super::{AgentRunner, AgentSupervision};
use crate::agent_supervisor::SpawnHost;

impl AgentRunner {
    pub(super) async fn cancel_child(&self, agent_id: AgentId) {
        let Some(supervision) = &self.supervision else {
            tracing::warn!(?agent_id, "CancelChild requested without a supervisor");
            return;
        };
        if let Err(error) = supervision.supervisor.cancel_child(agent_id).await {
            tracing::warn!(?agent_id, %error, "failed to cancel child agent");
        }
    }

    pub(super) async fn spawn_agent(
        &mut self,
        spec: SpawnAgentSpec,
    ) -> Result<ToolSpawnOutcome, String> {
        let Some(supervision) = self.supervision.clone() else {
            return Err("SpawnAgent requested without a supervisor".to_string());
        };
        let awaiting = matches!(spec.mode, SpawnMode::AwaitResult);
        let task = spec.task.clone();
        let child_id = supervision
            .supervisor
            .spawn_child(
                SpawnHost {
                    parent: &self.agent,
                    backend: self.backend.clone(),
                    integrations: supervision.integrations.as_ref(),
                    tools: self.tool_registry.clone(),
                    workspace: self.workspace.clone(),
                    events: self.event_sink.clone(),
                    scheduler: self.scheduler.clone(),
                    parent_commands: self.task.commands_tx.clone(),
                },
                spec,
            )
            .await
            .map_err(|error| error.to_string())?;

        // M5: kick off the child's initial task, if one was given — before
        // anything might block on `await_child` below, so an `AwaitResult`
        // spawn doesn't wait on a child that was never told to do anything.
        // Pre-M5 callers that leave `task: None` keep the original behavior:
        // an idle child, started by whatever Rust orchestration code sends
        // it a command later.
        if let Some(task) = task {
            start_child_task(supervision.supervisor.as_ref(), child_id, task).await;
        }

        let effects = self.apply_and_publish(AgentCommand::ChildSpawned {
            agent_id: child_id,
            awaiting,
        });
        Box::pin(self.dispatch_effects(effects)).await;
        let awaited = if awaiting {
            Some(self.await_child(&supervision, child_id).await)
        } else {
            None
        };
        Ok(ToolSpawnOutcome { child_id, awaited })
    }

    /// Waits for an `AwaitResult` child; a failed wait fails the child in
    /// the core too.
    pub(super) async fn await_child(
        &mut self,
        supervision: &AgentSupervision,
        child_id: AgentId,
    ) -> Result<AgentResult, String> {
        let error = match supervision.supervisor.await_child(child_id).await {
            Ok(result) => return Ok(result),
            Err(error) => error,
        };
        let effects = self.apply_and_publish(AgentCommand::ChildFailed {
            agent_id: child_id,
            error: AgentError {
                message: error.to_string(),
                code: "CHILD_AWAIT_FAILED".into(),
                details: None,
            },
        });
        Box::pin(self.dispatch_effects(effects)).await;
        Err(error.to_string())
    }

    /// M5: `agent.spawn`'s implementation. Always resolves via
    /// `AgentCommand::ToolCompleted` — never `ToolFailed` — for any spawn
    /// rejection, whether that's bad arguments, a tool it can't be granted,
    /// or the supervisor rejecting a budget/depth/capability request
    /// (`SupervisorError`, e.g. `SpawnNotAllowed`, budget escalation, depth
    /// exceeded). All of these are meaningful, actionable information for
    /// the calling model (e.g. "your budget request escalates past your
    /// own ceiling" is something a model should see and correct), unlike
    /// `ToolError`'s coarse fixed enum, which has no room for a real
    /// message and exists for infrastructure failures the model can't
    /// reason about — mirroring how every other tool in this workspace
    /// reports its own domain errors as `ToolResult { is_error: true,
    /// output: <message> }` rather than `ToolFailed`.
    pub(super) async fn execute_spawn_tool(
        &mut self,
        call_id: ToolCallId,
        arguments: &serde_json::Value,
    ) {
        let result = match crate::spawn_tool::build_spawn_spec(arguments, &self.agent) {
            Ok((mut spec, warnings)) => {
                spec.origin_tool_call_id = Some(call_id);
                match self.spawn_agent(spec).await {
                    Ok(outcome) => spawned_child_result(call_id, &outcome, &warnings),
                    Err(message) => {
                        tracing::warn!(%message, "agent.spawn tool call could not spawn a child");
                        spawn_refused_result(call_id, message)
                    }
                }
            }
            Err(message) => spawn_refused_result(call_id, message),
        };
        let effects = self.apply_and_publish(AgentCommand::ToolCompleted { call_id, result });
        Box::pin(self.dispatch_effects(effects)).await;
    }
}
