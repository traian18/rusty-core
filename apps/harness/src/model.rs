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

/// One line in the activity log (`Ctrl+L` / `/log`): a raw, chronological
/// record of every event the agent emitted, independent of the curated
/// transcript above. Unlike `TranscriptBlock`, this is never folded or
/// mutated in place — each event gets exactly one entry, in arrival order,
/// so it stays a faithful low-level trace of "what actually happened" even
/// for event kinds (state transitions, backend request start, permission
/// requests, usage updates) the curated transcript doesn't render at all.
#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    /// The event's `agent_sequence` — monotonic per agent, so entries are
    /// already in causal order without needing a wall-clock timestamp.
    pub sequence: u64,
    /// A human-readable one-line summary of the event.
    pub text: String,
}
