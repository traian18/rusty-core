//! Recording a requested call, and putting an admitted one in motion.

use rusty_protocol::{
    commands::AgentStatus,
    effects::{AgentEffect, PermissionRequest, ToolRequest},
    events::AgentEvent,
    tools::{PermissionMode, ToolCall},
};

use super::Approval;
use crate::{agent::Agent, agent_state::PendingToolCall};

impl Agent {
    /// Puts the call in the transcript and the pending set, and announces it.
    pub(super) fn record_tool_request(&mut self, call: &ToolCall) -> Vec<AgentEffect> {
        self.push_tool_use(call.clone());
        let started_at = self.next_timestamp();
        self.state.pending_tools.insert(
            call.id,
            PendingToolCall {
                call: call.clone(),
                started_at,
            },
        );
        vec![AgentEffect::Emit {
            event: AgentEvent::ToolCallRequested { call: call.clone() },
        }]
    }

    /// Runs the call now, or asks the user first.
    pub(super) fn start_admitted_call(
        &mut self,
        from: AgentStatus,
        call: ToolCall,
        approval: Approval,
    ) -> Vec<AgentEffect> {
        match approval {
            Approval::Allow => self.execute_now(from, call),
            Approval::Ask => self.ask_permission(from, call),
        }
    }

    fn execute_now(&mut self, from: AgentStatus, call: ToolCall) -> Vec<AgentEffect> {
        self.state.status = AgentStatus::Executing;
        vec![
            Self::state_changed(from, AgentStatus::Executing),
            AgentEffect::ExecuteTool {
                request: ToolRequest {
                    call,
                    permission: PermissionMode::Allow,
                },
            },
        ]
    }

    fn ask_permission(&mut self, from: AgentStatus, call: ToolCall) -> Vec<AgentEffect> {
        self.state.status = AgentStatus::WaitingForPermission;
        let permission_id = self.next_permission_id();
        self.state
            .pending_permissions
            .insert(permission_id, call.id);
        let request = PermissionRequest {
            id: permission_id,
            tool_call: call,
            agent_id: self.id,
        };
        vec![
            Self::state_changed(from, AgentStatus::WaitingForPermission),
            AgentEffect::Emit {
                event: AgentEvent::PermissionRequested {
                    request: request.clone(),
                },
            },
            AgentEffect::RequestPermission { request },
        ]
    }
}
