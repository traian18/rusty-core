//! Status changes and failing a run.

use rusty_protocol::commands::{AgentError, AgentResult, AgentStatus};
use rusty_protocol::effects::AgentEffect;
use rusty_protocol::events::AgentEvent;

use crate::agent::Agent;

impl Agent {
    pub(super) fn state_changed(from: AgentStatus, to: AgentStatus) -> AgentEffect {
        AgentEffect::Emit {
            event: AgentEvent::StateChanged { from, to },
        }
    }

    pub(super) fn fail(&mut self, code: &str, message: String) -> Vec<AgentEffect> {
        self.discard_trailing_empty_assistant_message();
        let from = self.state.status;
        let error = AgentError {
            message,
            code: code.into(),
            details: None,
        };
        self.state.status = AgentStatus::Failed;
        self.state.active_run = None;
        self.state.backend_in_flight = false;
        self.state.last_error = Some(error.clone());
        self.usage.runs = self.usage.runs.saturating_add(1);
        vec![
            Self::state_changed(from, AgentStatus::Failed),
            AgentEffect::Emit {
                event: AgentEvent::Failed {
                    error: error.clone(),
                },
            },
            AgentEffect::FinishRun {
                result: AgentResult {
                    summary: error.message,
                    usage: self.terminal_usage_summary(),
                    gate_passed: None,
                },
            },
        ]
    }
}
