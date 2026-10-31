//! Starting a tool call, once the agent's permission rules allow it.

use rusty_protocol::commands::AgentCommand;
use rusty_protocol::effects::ToolRequest;
use rusty_protocol::ids::ToolCallId;

use super::mailbox::deliver;
use super::tool_run::ToolRun;
use super::AgentRunner;

impl AgentRunner {
    /// Executes a tool call and reports the result to the agent's mailbox.
    pub(super) async fn execute_tool(&mut self, request: ToolRequest) {
        let call_id = request.call.id;
        let name = request.call.name;
        let arguments = request.call.arguments;

        if !self.tool_call_permitted(call_id, &name, &arguments) {
            let error = rusty_protocol::tools::ToolError::PermissionDenied;
            deliver(
                &self.task.commands_tx,
                AgentCommand::ToolFailed { call_id, error },
            )
            .await;
            return;
        }

        // M5: `agent.spawn` is not a registered `ToolExecutor` — its
        // implementation needs `&mut self` (to call `spawn_agent`, which
        // applies `ChildSpawned`/`ChildFailed` against the deterministic
        // core) that the generic path below cannot give it, since normal
        // tool execution runs in a detached `tokio::spawn`'d task with no
        // access back into the runner. Intercepted here, before any
        // registry lookup, and always routed through the exact same
        // `spawn_agent` the plain `AgentEffect::SpawnAgent` path already
        // uses — a model can no more bypass parent budgets, tool
        // delegation rules, or depth limits through this tool than existing
        // Rust-orchestrated spawning could.
        if name == crate::spawn_tool::AGENT_SPAWN_TOOL_NAME {
            self.execute_spawn_tool(call_id, &arguments).await;
            return;
        }

        let token = self.cancel.child_token();
        self.tool_tokens.insert(call_id, token.clone());
        let run = ToolRun {
            call_id,
            executor: self.tool_registry.get_executor(&name),
            name,
            arguments,
            scheduler: self.scheduler.clone(),
            token,
            commands: self.task.commands_tx.clone(),
            session_id: self.agent.session_id.to_string(),
        };
        tokio::spawn(run.run());
    }

    /// Recheck the actual capability before every route, including spawn.
    /// `request.permission` is an effect field, not an authorization grant.
    pub(super) fn tool_call_permitted(
        &self,
        call_id: ToolCallId,
        name: &str,
        arguments: &serde_json::Value,
    ) -> bool {
        use crate::permissions::{PermissionOutcome, PermissionPolicy};
        match PermissionPolicy.evaluate(&self.agent.capabilities, name) {
            PermissionOutcome::Allow => true,
            PermissionOutcome::Denied(_) => false,
            PermissionOutcome::RequiresApproval(_) => self.call_approved(call_id, name, arguments),
        }
    }

    /// The call needing approval is the one that was pending, and no
    /// permission request for it is still open.
    pub(super) fn call_approved(
        &self,
        call_id: ToolCallId,
        name: &str,
        arguments: &serde_json::Value,
    ) -> bool {
        let state = &self.agent.state;
        let is_pending_call = state.pending_tools.get(&call_id).is_some_and(|pending| {
            pending.call.name == name && pending.call.arguments == *arguments
        });
        is_pending_call && !state.pending_permissions.values().any(|id| *id == call_id)
    }
}
