//! A tool call's outcome, and what happens after all calls of a turn are in.

use rusty_protocol::commands::AgentStatus;
use rusty_protocol::effects::AgentEffect;
use rusty_protocol::events::AgentEvent;
use rusty_protocol::ids::ToolCallId;
use rusty_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use rusty_protocol::tools::{ToolCall, ToolError, ToolResult, ToolResultSummary};

use crate::agent::Agent;
use crate::behavior::{RuleEvent, RuleStop, ToolEvent};

/// Consecutive timed-out tool calls after which a run fails with `TOOL_TIMEOUT`.
pub const MAX_CONSECUTIVE_TOOL_TIMEOUTS: u32 = 3;

impl Agent {
    pub(super) fn tool_completed(
        &mut self,
        call_id: ToolCallId,
        result: ToolResult,
    ) -> Vec<AgentEffect> {
        let Some(pending) = self.state.pending_tools.remove(&call_id) else {
            return Vec::new();
        };
        self.state.consecutive_tool_timeouts = 0;
        self.state
            .pending_permissions
            .retain(|_, id| *id != call_id);
        self.record_tool_result(
            pending.call,
            true,
            result.is_error,
            serde_json::to_string(&result.output).unwrap_or_default(),
        )
    }

    pub(super) fn tool_failed(
        &mut self,
        call_id: ToolCallId,
        error: ToolError,
    ) -> Vec<AgentEffect> {
        let Some(pending) = self.state.pending_tools.remove(&call_id) else {
            return Vec::new();
        };
        self.state
            .pending_permissions
            .retain(|_, id| *id != call_id);
        // A refused call never ran; one that failed while running did.
        let executed = !matches!(
            error,
            ToolError::PermissionDenied | ToolError::Denied { .. }
        );
        if matches!(error, ToolError::Timeout) {
            self.state.consecutive_tool_timeouts += 1;
        } else {
            self.state.consecutive_tool_timeouts = 0;
        }
        let preview = match error {
            ToolError::Denied { reason } => format!("Denied: {reason}"),
            ToolError::Timeout => format!(
                "Tool call `{}` timed out and was cancelled. Try a narrower request or a different approach.",
                pending.call.name
            ),
            other => format!("{other:?}"),
        };
        self.record_tool_result(pending.call, executed, true, preview)
    }

    /// Records a tool call's outcome: lets the rules see it, adds the result to the transcript,
    /// announces it, then decides what the agent does next.
    pub(super) fn record_tool_result(
        &mut self,
        call: ToolCall,
        executed: bool,
        has_error: bool,
        mut preview: String,
    ) -> Vec<AgentEffect> {
        let call_id = call.id;
        let mut effects = Vec::new();
        let stop = if executed {
            self.run_post_tool_rules(&call, has_error, &mut preview, &mut effects)
        } else {
            None
        };
        self.append_pending_notes(call_id, &mut preview);
        self.usage.tool_calls = self.usage.tool_calls.saturating_add(1);
        self.push_tool_result_message(call_id, has_error, &preview);
        effects.push(AgentEffect::Emit {
            event: AgentEvent::ToolCallCompleted {
                call_id,
                result: ToolResultSummary {
                    has_error,
                    output_preview: preview,
                },
            },
        });
        effects.extend(self.next_after_result(stop));
        effects
    }

    /// Lets the profile's rules see the outcome. Rules may rewrite the preview or stop the run.
    fn run_post_tool_rules(
        &mut self,
        call: &ToolCall,
        has_error: bool,
        preview: &mut String,
        effects: &mut Vec<AgentEffect>,
    ) -> Option<RuleStop> {
        self.state
            .behavior
            .note_executed_with(&call.name, has_error);
        let event = if has_error {
            RuleEvent::PostToolUseFailure
        } else {
            RuleEvent::PostToolUse
        };
        let outcome = self.state.behavior.evaluate(
            event,
            Some(ToolEvent {
                call,
                result: Some(preview),
            }),
        );
        effects.extend(self.apply_rule_outcome(&outcome, Some(call.id)));
        for (_, rewrite) in &outcome.rewrite_result {
            rewrite.apply(preview);
        }
        outcome.stop
    }

    /// Adds the notes earlier steps queued for this call (such as a rewrite notice) to its result.
    fn append_pending_notes(&mut self, call_id: ToolCallId, preview: &mut String) {
        self.state
            .behavior
            .run
            .pending_results
            .retain(|(pending_call, text)| {
                if *pending_call != call_id {
                    return true;
                }
                preview.push_str("\n\n");
                preview.push_str(text);
                false
            });
    }

    fn push_tool_result_message(&mut self, call_id: ToolCallId, has_error: bool, preview: &str) {
        let id = self.next_message_id();
        let created_at = self.next_timestamp();
        self.state.messages.push(AgentMessage {
            id,
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult {
                call_id,
                result: ToolResultSummary {
                    has_error,
                    output_preview: preview.to_owned(),
                },
            }],
            created_at,
        });
    }

    /// The result is already in the transcript, so it stays valid whichever way this goes: a rule
    /// stop ends the run, repeated timeouts fail it, otherwise the turn continues.
    fn next_after_result(&mut self, stop: Option<RuleStop>) -> Vec<AgentEffect> {
        let timed_out_too_often =
            self.state.consecutive_tool_timeouts >= MAX_CONSECUTIVE_TOOL_TIMEOUTS;
        match stop {
            None if timed_out_too_often => {
                let count = self.state.consecutive_tool_timeouts;
                self.state.consecutive_tool_timeouts = 0;
                self.fail(
                    "TOOL_TIMEOUT",
                    format!("{count} consecutive tool calls timed out"),
                )
            }
            Some(stop) => self.behavior_stop(&stop),
            None => self.continue_after_tools(),
        }
    }

    pub(super) fn continue_after_tools(&mut self) -> Vec<AgentEffect> {
        let mut effects = Vec::new();
        if self.state.pending_tools.is_empty() && !self.state.backend_in_flight {
            if let Some(run_id) = self.state.active_run {
                match self.begin_turn() {
                    Ok(turn_effects) => effects.extend(turn_effects),
                    Err(failure) => {
                        effects.extend(failure);
                        return effects;
                    }
                }
                let from = self.state.status;
                self.state.status = AgentStatus::WaitingForBackend;
                let request = self.execution_request(run_id);
                if from != AgentStatus::WaitingForBackend {
                    effects.push(Self::state_changed(from, AgentStatus::WaitingForBackend));
                }
                effects.push(AgentEffect::ExecuteBackend { request });
            }
        }
        effects
    }
}
