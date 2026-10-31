//! Asking the completion gate whether the agent may finish.

use rusty_protocol::commands::AgentCommand;

use super::mailbox::deliver;

use super::AgentRunner;

impl AgentRunner {
    /// Run the completion gate's evaluators off the mailbox loop and report
    /// the verdicts back as `CompletionEvaluated`, like a tool result.
    pub(super) fn evaluate_completion(
        &mut self,
        request: rusty_protocol::effects::CompletionEvaluationRequest,
    ) {
        let token = self.cancel.child_token();
        if let Some(previous) = self.evaluation_tokens.insert(request.run_id, token.clone()) {
            previous.cancel();
        }
        let context = crate::completion_gate::GateContext {
            backend: self.backend.clone(),
            tool_registry: self.tool_registry.clone(),
            workspace: self.workspace.clone(),
            commands_trusted: self.agent.state.behavior.commands_trusted,
            tool_permissions: self
                .agent
                .capabilities
                .tools
                .tools
                .values()
                .filter(|capability| capability.policy.enabled)
                .map(|capability| {
                    (
                        capability.descriptor.name.clone(),
                        capability.policy.permission.clone(),
                    )
                })
                .collect(),
            params: self
                .agent
                .state
                .behavior
                .profile
                .execution_params(&self.agent.state.execution_params),
            session_id: self.agent.session_id.to_string(),
        };
        let commands = self.task.commands_tx.clone();
        tokio::spawn(async move {
            let (verdicts, usage) =
                crate::completion_gate::evaluate(&context, &request, &token).await;
            // A cancelled evaluation still spent its requests; report them
            // so usage stays truthful (the core ignores the stale verdicts).
            if token.is_cancelled() && usage.is_empty() {
                return;
            }
            let evaluated = AgentCommand::CompletionEvaluated {
                run_id: request.run_id,
                attempt: request.attempt,
                verdicts,
                usage,
            };
            deliver(&commands, evaluated).await;
        });
    }
}
