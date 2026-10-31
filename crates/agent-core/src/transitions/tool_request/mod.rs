//! A tool call the model asks for: run it, ask the user first, or refuse it.
//!
//! [`Agent::tool_requested`] reads as the decision in order. Each step either lets the call
//! through or ends it with the effects that refuse it, so a refusal never has to be unwound.

mod admit;
mod deny;
mod rewrite;
mod start;

use rusty_protocol::{
    commands::{AgentStatus, PermissionDecision},
    effects::{AgentEffect, ToolRequest},
    ids::PermissionId,
    tools::{PermissionMode, ToolCall, ToolError},
};

use crate::agent::Agent;

/// How an admitted call proceeds.
pub(super) enum Approval {
    /// Run it now.
    Allow,
    /// Wait for the user to approve it.
    Ask,
}

impl Agent {
    /// The model asked for a tool: decide whether it runs, needs approval, or is refused.
    pub(super) fn tool_requested(&mut self, call: ToolCall) -> Vec<AgentEffect> {
        let from = self.state.status;
        let mut effects = self.record_tool_request(&call);
        match self.admit_tool_call(call, &mut effects) {
            Ok((call, approval)) => effects.extend(self.start_admitted_call(from, call, approval)),
            Err(refusal) => effects.extend(refusal),
        }
        effects
    }

    /// The user answered an approval prompt: run the call or fail it.
    pub(super) fn permission_resolved(
        &mut self,
        id: PermissionId,
        decision: PermissionDecision,
    ) -> Vec<AgentEffect> {
        let Some(call_id) = self.state.pending_permissions.remove(&id) else {
            return Vec::new();
        };
        let Some(call) = self
            .state
            .pending_tools
            .get(&call_id)
            .map(|pending| pending.call.clone())
        else {
            return Vec::new();
        };
        match decision {
            PermissionDecision::Approved => {
                self.state.status = AgentStatus::Executing;
                vec![
                    Self::state_changed(AgentStatus::WaitingForPermission, AgentStatus::Executing),
                    AgentEffect::ExecuteTool {
                        request: ToolRequest {
                            call,
                            permission: PermissionMode::Allow,
                        },
                    },
                ]
            }
            PermissionDecision::Denied => self.tool_failed(call_id, ToolError::PermissionDenied),
        }
    }
}
