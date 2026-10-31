//! Finishing a run: the completion gate and the final answer.
//!
//! When the model ends a turn without tool calls, `propose_completion` decides whether the run
//! may finish. With no gate it does. With one, `gate` judges the failed checks (declarative ones
//! at once, evaluator ones after `evaluated` brings their verdicts back).

mod evaluated;
mod gate;
mod propose;

use rusty_protocol::{
    commands::{AgentResult, AgentStatus},
    effects::AgentEffect,
    events::{AgentEvent, AgentOutcome},
    messages::{ContentBlock, MessageRole},
};

use crate::agent::Agent;

/// Recent transcript messages handed to `command` gate evaluators.
const COMMAND_TRANSCRIPT_MESSAGES: usize = 20;

impl Agent {
    /// Ends the run successfully and returns the agent to `Idle`.
    pub(super) fn finish_success(
        &mut self,
        summary: String,
        gate_passed: Option<bool>,
    ) -> Vec<AgentEffect> {
        let from = self.state.status;
        self.state.status = AgentStatus::Idle;
        self.state.active_run = None;
        self.usage.runs = self.usage.runs.saturating_add(1);
        vec![
            Self::state_changed(from, AgentStatus::Idle),
            AgentEffect::Emit {
                event: AgentEvent::Completed {
                    outcome: AgentOutcome::Success,
                },
            },
            AgentEffect::FinishRun {
                result: AgentResult {
                    summary,
                    usage: self.terminal_usage_summary(),
                    gate_passed,
                },
            },
        ]
    }

    /// The text of the last assistant message, which is the run's answer.
    pub(super) fn final_response_text(&self) -> String {
        self.state
            .messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::Assistant)
            .map(|message| {
                message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default()
    }
}
