//! The model's proposed final answer.

use rusty_protocol::{
    commands::AgentStatus,
    effects::{AgentEffect, CompletionCheckSpec, CompletionEvaluationRequest},
    ids::RunId,
};

use super::COMMAND_TRANSCRIPT_MESSAGES;
use crate::{
    agent::Agent,
    behavior::{CompletionGate, EvaluatorSpec},
};

/// A check that failed: its id and the feedback the model is shown.
pub(super) type CheckFailure = (String, String);

impl Agent {
    /// The model produced a turn with no tool calls. Without a gate the run finishes. With one,
    /// declarative checks are decided now; evaluator checks are handed to the runtime and the
    /// agent waits in `Verifying`.
    pub(in crate::transitions) fn propose_completion(
        &mut self,
        run_id: RunId,
        summary: String,
    ) -> Vec<AgentEffect> {
        let profile = self.state.behavior.profile.clone();
        let Some(gate) = &profile.profile.completion_gate else {
            return self.finish_success(summary, None);
        };
        self.note_gate_attempt();

        let mut failures = self.failed_declarative_checks(gate);
        let (evaluators, unpreparable) = evaluator_specs(gate);
        failures.extend(unpreparable);

        let evaluators_have_nothing_to_do = failures.is_empty() && evaluators.is_empty();
        if !failures.is_empty() || evaluators_have_nothing_to_do {
            return self.gate_decided(run_id, failures, summary);
        }
        self.start_verifying(run_id, gate, evaluators)
    }

    fn note_gate_attempt(&mut self) {
        self.state.behavior.run.gate_attempts += 1;
        self.state.behavior.run.final_status =
            Some(crate::behavior::final_status(&self.final_response_text()));
    }

    fn failed_declarative_checks(&self, gate: &CompletionGate) -> Vec<CheckFailure> {
        gate.checks
            .iter()
            .filter_map(|check| {
                let condition = check.require.as_ref()?;
                (!self.state.behavior.condition_holds(condition))
                    .then(|| (check.id.clone(), check.feedback.clone().unwrap_or_default()))
            })
            .collect()
    }

    /// Hands the evaluator checks to the runtime and waits for their verdicts.
    fn start_verifying(
        &mut self,
        run_id: RunId,
        gate: &CompletionGate,
        evaluators: Vec<CompletionCheckSpec>,
    ) -> Vec<AgentEffect> {
        let transcript = self.transcript_tail(longest_transcript_needed(gate));
        let final_response = self.final_response_text();
        let from = self.state.status;
        self.state.status = AgentStatus::Verifying;
        vec![
            Self::state_changed(from, AgentStatus::Verifying),
            AgentEffect::EvaluateCompletion {
                request: CompletionEvaluationRequest {
                    run_id,
                    attempt: self.state.behavior.run.gate_attempts,
                    checks: evaluators,
                    final_response,
                    transcript,
                },
            },
        ]
    }

    fn transcript_tail(&self, count: usize) -> Vec<rusty_protocol::messages::AgentMessage> {
        let messages = &self.state.messages;
        messages[messages.len().saturating_sub(count)..].to_vec()
    }
}

/// The evaluator checks as the runtime will run them. A spec that cannot be prepared is reported
/// as a failed check, never dropped: dropping it would let the answer pass unchecked.
fn evaluator_specs(gate: &CompletionGate) -> (Vec<CompletionCheckSpec>, Vec<CheckFailure>) {
    let mut specs = Vec::new();
    let mut unpreparable = Vec::new();
    for check in &gate.checks {
        let Some(evaluator) = &check.evaluator else {
            continue;
        };
        match serde_json::to_value(evaluator) {
            Ok(evaluator) => specs.push(CompletionCheckSpec {
                id: check.id.clone(),
                evaluator,
            }),
            Err(error) => unpreparable.push((
                check.id.clone(),
                format!("the check could not be prepared: {error}"),
            )),
        }
    }
    (specs, unpreparable)
}

/// How many recent messages the most demanding evaluator wants to see.
fn longest_transcript_needed(gate: &CompletionGate) -> usize {
    gate.checks
        .iter()
        .filter_map(|check| match &check.evaluator {
            Some(
                EvaluatorSpec::Model {
                    transcript_messages,
                    ..
                }
                | EvaluatorSpec::Agent {
                    transcript_messages,
                    ..
                },
            ) => Some(*transcript_messages as usize),
            Some(EvaluatorSpec::Command { .. }) => Some(COMMAND_TRANSCRIPT_MESSAGES),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}
