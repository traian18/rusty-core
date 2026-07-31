//! Durable mutable state owned by an agent.

use std::collections::HashMap;

use harness_protocol::commands::{AgentError, AgentOperation, AgentStatus};
use harness_protocol::ids::{AgentId, PermissionId, RunId, Timestamp, ToolCallId};
use harness_protocol::messages::AgentMessage;
use harness_protocol::tools::ToolCall;

#[derive(Debug, Clone)]
pub struct PendingToolCall {
    pub call: ToolCall,
    pub started_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct AgentState {
    pub status: AgentStatus,
    pub current_operation: Option<AgentOperation>,
    pub system_prompt: String,
    pub messages: Vec<AgentMessage>,
    pub active_run: Option<RunId>,
    pub pending_tools: HashMap<ToolCallId, PendingToolCall>,
    /// Exact correlation from a permission request to its pending tool call.
    pub pending_permissions: HashMap<PermissionId, ToolCallId>,
    pub children: Vec<AgentId>,
    pub last_error: Option<AgentError>,
    /// Monotonic source for IDs and timestamps created by deterministic transitions.
    pub transition_sequence: u64,
    /// Nesting depth in the agent tree: `0` for a root agent,
    /// `parent.depth + 1` for a child. Read-only from the core's
    /// perspective; the runtime supervisor enforces depth limits.
    pub depth: u32,
}
