use harness_protocol::{
    events::AgentOutcome,
    ids::{AgentId, MessageId, PermissionId, ToolCallId},
};

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptBlock {
    UserMessage {
        text: String,
    },
    AssistantMessage {
        id: MessageId,
        agent_id: AgentId,
        text: String,
        reasoning: String,
        complete: bool,
    },
    ToolCall {
        id: ToolCallId,
        agent_id: AgentId,
        name: String,
        arguments: serde_json::Value,
        state: ToolCallState,
    },
    Permission {
        id: PermissionId,
        tool_call_id: ToolCallId,
        tool_name: String,
        decision: Option<PermissionDisplayDecision>,
    },
    ChildAgent {
        agent_id: AgentId,
        outcome: Option<AgentOutcome>,
    },
    SystemNotice {
        text: String,
    },
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolCallState {
    Requested,
    Running,
    Progress { status: String, fraction: f64 },
    Succeeded { preview: String },
    Failed { preview: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDisplayDecision {
    Approved,
    Denied,
}
