//! Acting on the completion gate's verdict: accept the answer, send it back, or give up.

use rusty_protocol::{
    commands::AgentStatus,
    effects::AgentEffect,
    events::AgentEvent,
    ids::RunId,
    messages::{AgentMessage, ContentBlock, MessageRole},
};

use super::propose::CheckFailure;
use crate::{
    agent::Agent,
    behavior::{system_reminder, CompletionGate, OnExhausted},
};

/// What the gate decides about an answer that has been checked.
enum GateStep {
    /// Nothing is left to fix, or only checks that waive themselves once the budget is spent.
    Accept,
    /// The model still has continuations: tell it what to fix and ask again.
    SendBack,
    /// Failing, and no continuations are left.
    Exhausted,
}

impl Agent {
    /// Applies the gate's decision for an answer whose checks produced `failures`.
    pub(in crate::transitions) fn gate_decided(
        &mut self,
        run_id: RunId,
        failures: Vec<CheckFailure>,
        summary: String,
    ) -> Vec<AgentEffect> {
        let profile = self.state.behavior.profile.clone();
        let Some(gate) = &profile.profile.completion_gate else {
            return self.finish_success(summary, None);
        };
        let failed_checks: Vec<String> = failures.iter().map(|(id, _)| id.clone()).collect();
        let step = self.gate_step(gate, &failed_checks);
        let mut effects = vec![self.gate_evaluated(&step, &failed_checks)];
        match step {
            GateStep::Accept => effects.extend(self.finish_success(summary, Some(true))),
            GateStep::Exhausted => {
                effects.extend(self.end_exhausted_gate(gate, &failed_checks, summary));
            }
            GateStep::SendBack => {
                effects.extend(self.send_answer_back(run_id, &profile.reference(), &failures));
            }
        }
        effects
    }

    fn gate_step(&self, gate: &CompletionGate, failed_checks: &[String]) -> GateStep {
        if failed_checks.is_empty() {
            return GateStep::Accept;
        }
        let run = &self.state.behavior.run;
        if run.gate_continuations < gate.max_continuations && !run.final_turn {
            return GateStep::SendBack;
        }
        let waived = |id: &String| {
            gate.checks
                .iter()
                .any(|check| &check.id == id && check.waive_when_exhausted)
        };
        if failed_checks.iter().all(waived) {
            GateStep::Accept
        } else {
            GateStep::Exhausted
        }
    }

    fn gate_evaluated(&self, step: &GateStep, failed_checks: &[String]) -> AgentEffect {
        AgentEffect::Emit {
            event: AgentEvent::CompletionGateEvaluated {
                attempt: self.state.behavior.run.gate_attempts,
                passed: matches!(step, GateStep::Accept),
                continuing: matches!(step, GateStep::SendBack),
                failed_checks: failed_checks.to_vec(),
            },
        }
    }

    /// The gate still fails with no continuations left: accept the answer as it is, or fail the
    /// run, as the profile says.
    fn end_exhausted_gate(
        &mut self,
        gate: &CompletionGate,
        failed_checks: &[String],
        summary: String,
    ) -> Vec<AgentEffect> {
        match gate.on_exhausted.unwrap_or(OnExhausted::Accept) {
            OnExhausted::Accept => self.finish_success(summary, Some(false)),
            OnExhausted::Fail => self.fail(
                "GATE_EXHAUSTED",
                format!(
                    "completion gate still failing after {} continuation(s): {}",
                    self.state.behavior.run.gate_continuations,
                    failed_checks.join(", ")
                ),
            ),
        }
    }

    /// Tells the model what to fix and asks it again.
    fn send_answer_back(
        &mut self,
        run_id: RunId,
        profile_reference: &impl std::fmt::Display,
        failures: &[CheckFailure],
    ) -> Vec<AgentEffect> {
        self.state.behavior.run.gate_continuations += 1;
        self.push_gate_rejection(profile_reference, failures);
        let mut effects = match self.begin_turn() {
            Ok(effects) => effects,
            Err(failure) => return failure,
        };
        let from = self.state.status;
        self.state.status = AgentStatus::WaitingForBackend;
        let request = self.execution_request(run_id);
        effects.push(Self::state_changed(from, AgentStatus::WaitingForBackend));
        effects.push(AgentEffect::ExecuteBackend { request });
        effects
    }

    fn push_gate_rejection(
        &mut self,
        profile_reference: &impl std::fmt::Display,
        failures: &[CheckFailure],
    ) {
        let mut text = String::from(
            "Your answer was not accepted yet. Address the following before finishing:",
        );
        for (id, feedback) in failures {
            text.push_str(&format!("\n- [{id}] {feedback}"));
        }
        let id = self.next_message_id();
        let created_at = self.next_timestamp();
        self.state.messages.push(AgentMessage {
            id,
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: system_reminder(&format!("gate:{profile_reference}"), &text),
            }],
            created_at,
        });
    }
}
