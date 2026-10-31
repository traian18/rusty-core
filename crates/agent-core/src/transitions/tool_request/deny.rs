//! Refusing a call before it runs.

use rusty_protocol::{effects::AgentEffect, events::AgentEvent, ids::ToolCallId, tools::ToolError};

use crate::agent::Agent;

impl Agent {
    /// Refuse a tool call before execution, telling the model why.
    pub(super) fn deny_tool(
        &mut self,
        call_id: ToolCallId,
        rule_id: Option<String>,
        reason: String,
    ) -> Vec<AgentEffect> {
        let mut effects = vec![AgentEffect::Emit {
            event: AgentEvent::ToolCallDenied {
                call_id,
                rule_id,
                reason: reason.clone(),
            },
        }];
        effects.extend(self.tool_failed(call_id, ToolError::Denied { reason }));
        effects
    }
}
