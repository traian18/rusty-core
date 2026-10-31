//! How a run ends and what it leaves behind: the final result, a fatal failure, persisted mutations.

use rusty_agent_core::transcript::validate_transcript;

use rusty_protocol::commands::{AgentError, AgentResult, AgentStatus};
use rusty_protocol::effects::SessionMutation;

use super::AgentRunner;

use crate::session_runtime::stored_agent_state;

impl AgentRunner {
    pub(super) fn record_final_result(&mut self, result: AgentResult) {
        self.final_result = Some(match self.agent.state.status {
            AgentStatus::Failed => {
                Err(self
                    .agent
                    .state
                    .last_error
                    .clone()
                    .unwrap_or_else(|| AgentError {
                        message: result.summary.clone(),
                        code: "FAILED".into(),
                        details: None,
                    }))
            }
            _ => Ok(result),
        });
    }

    pub(super) fn fail_fatally(&mut self, code: &str, message: String) {
        self.fatal_error = true;
        for (_, token) in self.backend_tokens.drain() {
            token.cancel();
        }
        for (_, token) in self.tool_tokens.drain() {
            token.cancel();
        }
        let error = AgentError {
            message,
            code: code.into(),
            details: None,
        };
        self.agent.state.status = AgentStatus::Failed;
        self.agent.state.active_run = None;
        self.agent.state.pending_tools.clear();
        self.agent.state.pending_permissions.clear();
        self.agent.state.last_error = Some(error.clone());
        self.final_result = Some(Err(error));
        self.publish_status();
    }

    /// Persists a session mutation through the injected session store.
    pub(super) async fn persist(&mut self, mutation: SessionMutation) {
        let Some(store) = &self.session_store else {
            tracing::warn!(?mutation, "Persist requested without a session store");
            return;
        };

        if let Err(error) = validate_transcript(&self.agent.state.messages) {
            tracing::error!(
                ?error,
                "refusing to persist mutation for invalid transcript"
            );
            return;
        }

        match mutation {
            SessionMutation::AppendEvent(envelope) => {
                if let Err(error) = store.append(envelope.into()).await {
                    tracing::error!(?error, "failed to append durable session event");
                }
            }
            SessionMutation::SaveSnapshot(payload) => {
                match serde_json::from_value::<rusty_session_store::DurableSessionSnapshot>(payload)
                {
                    Ok(snapshot) => {
                        if let Err(error) = store.save_snapshot(snapshot).await {
                            tracing::error!(?error, "failed to save session snapshot");
                        }
                    }
                    Err(error) => {
                        tracing::error!(?error, "dropping invalid session snapshot payload");
                    }
                }
            }
        }
    }

    pub(super) fn publish_status(&self) {
        // M4: `self_metrics()` correctly sums cost with "unknown poisons the
        // total" semantics (matching `UsageValue`'s existing token-count
        // discipline) — previously this call site duplicated a *buggy*
        // version of the same calculation inline, one that silently skipped
        // unknown-cost records instead of reporting the total as unknown.
        let child_usage = self
            .agent
            .usage
            .child_usage
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let self_metrics = self.agent.usage.self_metrics();
        let usage = rusty_agent_core::usage::compute_agent_usage_summary(
            self_metrics.clone(),
            &child_usage,
        );

        {
            let mut table = self.live_state.lock().expect("live_state mutex poisoned");
            let entry = table.entry(self.agent.id).or_default();
            entry.status = self.agent.state.status;
            entry.current_operation = self.agent.state.current_operation.clone();
            entry.last_error = self.agent.state.last_error.clone();
            entry.total_requests = self_metrics.total_requests;
            entry.total_cost_usd = self_metrics.total_cost;
            entry.usage = usage;
        }

        // RC-302: publish the durable projection so checkpoints are truthful.
        if let Some(projection) = &self.projection {
            projection
                .lock()
                .expect("projection mutex poisoned")
                .insert(self.agent.id, stored_agent_state(&self.agent));
        }
    }
}
