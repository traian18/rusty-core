//! Carrying out the effects the agent asks for, one by one.

use std::collections::HashMap;

use tokio_util::sync::CancellationToken;

use rusty_protocol::effects::AgentEffect;
use rusty_protocol::ids::ToolCallId;

use super::AgentRunner;

impl AgentRunner {
    /// Runs `effects` in order; whether the run finished. A failed emit
    /// finishes it and skips the effects after it.
    pub(super) async fn dispatch_effects(&mut self, effects: Vec<AgentEffect>) -> bool {
        let mut finish = false;
        for effect in effects {
            match self.dispatch_effect(effect).await {
                Dispatched::Continue => {}
                Dispatched::RunFinished => finish = true,
                Dispatched::Stop => return true,
            }
        }
        finish
    }

    pub(super) async fn dispatch_effect(&mut self, effect: AgentEffect) -> Dispatched {
        match effect {
            AgentEffect::Emit { event } => {
                if !self.emit(event).await {
                    return Dispatched::Stop;
                }
            }
            AgentEffect::FinishRun { result } => {
                self.record_final_result(result);
                return Dispatched::RunFinished;
            }
            AgentEffect::ExecuteBackend { request } => self.execute_backend(request).await,
            AgentEffect::ExecuteTool { request } => self.execute_tool(request).await,
            AgentEffect::SpawnAgent { spec } => {
                if let Err(message) = self.spawn_agent(spec).await {
                    tracing::warn!(%message, "SpawnAgent effect did not spawn a child");
                }
            }
            AgentEffect::RequestPermission { request } => {
                // The preceding PermissionRequested event is the single
                // host-facing delivery path. This effect deliberately
                // performs no second publication: the runner remains in
                // WaitingForPermission until the matching
                // PermissionResolved command arrives.
                debug_assert!(self
                    .agent
                    .state
                    .pending_permissions
                    .contains_key(&request.id));
            }
            AgentEffect::Persist { mutation } => self.persist(mutation).await,
            AgentEffect::EvaluateCompletion { request } => self.evaluate_completion(request),
            cancel @ (AgentEffect::CancelBackend { .. }
            | AgentEffect::CancelTool { .. }
            | AgentEffect::CancelChild { .. }
            | AgentEffect::CancelEvaluation { .. }) => self.dispatch_cancel(cancel).await,
        }
        Dispatched::Continue
    }

    /// The `Cancel*` effects [`Self::dispatch_effect`] routes here.
    pub(super) async fn dispatch_cancel(&mut self, effect: AgentEffect) {
        match effect {
            AgentEffect::CancelBackend { run_id } => {
                cancel_token(&mut self.backend_tokens, &run_id)
            }
            AgentEffect::CancelTool { call_id } => self.cancel_tool(call_id),
            AgentEffect::CancelEvaluation { run_id } => {
                cancel_token(&mut self.evaluation_tokens, &run_id)
            }
            AgentEffect::CancelChild { agent_id } => self.cancel_child(agent_id).await,
            _ => unreachable!("dispatch_effect routes only cancel effects here"),
        }
    }

    pub(super) fn cancel_tool(&mut self, call_id: ToolCallId) {
        cancel_token(&mut self.tool_tokens, &call_id);
    }
}

/// How dispatching one effect leaves the run.
pub(super) enum Dispatched {
    Continue,
    /// A `FinishRun` effect completed the run.
    RunFinished,
    /// An event could not be committed; the effects after it are skipped.
    Stop,
}

/// Cancel and forget the in-flight work `key` names, if any.
pub(super) fn cancel_token<K: std::hash::Hash + Eq>(
    tokens: &mut HashMap<K, CancellationToken>,
    key: &K,
) {
    if let Some(token) = tokens.remove(key) {
        token.cancel();
    }
}
