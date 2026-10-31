//! The verdicts of the gate's evaluator checks coming back.

use rusty_protocol::{
    commands::{AgentStatus, CheckVerdict, VerdictOutcome},
    effects::AgentEffect,
    events::AgentEvent,
    ids::RunId,
    usage::{AgentUsageSnapshot, UsageRecord},
};

use crate::{agent::Agent, behavior::ErrorPolicy};

impl Agent {
    pub(in crate::transitions) fn completion_evaluated(
        &mut self,
        run_id: RunId,
        attempt: u32,
        verdicts: Vec<CheckVerdict>,
        usage: Vec<UsageRecord>,
    ) -> Vec<AgentEffect> {
        // Evaluator requests were made either way, so they always count.
        let mut effects = Vec::new();
        if !usage.is_empty() {
            self.usage.records.extend(usage);
            effects.push(AgentEffect::Emit {
                event: AgentEvent::UsageUpdated {
                    usage: AgentUsageSnapshot {
                        agent_id: self.id.to_string(),
                        model: self.state.execution_params.model.clone(),
                        metrics: self.usage.self_metrics(),
                        timestamp: self.next_timestamp().to_rfc3339(),
                    },
                },
            });
        }
        // Stale or unexpected verdicts (cancelled, superseded) are ignored.
        if self.state.status != AgentStatus::Verifying
            || self.state.active_run != Some(run_id)
            || self.state.behavior.run.gate_attempts != attempt
        {
            return effects;
        }
        let profile = self.state.behavior.profile.clone();
        let Some(gate) = &profile.profile.completion_gate else {
            return Vec::new();
        };
        let failures = gate
            .checks
            .iter()
            .filter(|check| check.evaluator.is_some())
            .filter_map(|check| {
                let outcome = verdicts
                    .iter()
                    .find(|verdict| verdict.id == check.id)
                    .map(|verdict| verdict.outcome.clone())
                    .unwrap_or(VerdictOutcome::Error {
                        message: "no verdict was produced".into(),
                    });
                let feedback = match outcome {
                    VerdictOutcome::Pass => return None,
                    VerdictOutcome::Fail { feedback } => feedback,
                    VerdictOutcome::Error { .. } if check.error_policy == ErrorPolicy::Pass => {
                        return None
                    }
                    VerdictOutcome::Error { message } => {
                        format!("the check could not be evaluated: {message}")
                    }
                };
                Some((check.id.clone(), check.feedback.clone().unwrap_or(feedback)))
            })
            .collect();
        let summary = "Run completed: end_turn".to_string();
        effects.extend(self.gate_decided(run_id, failures, summary));
        effects
    }
}
