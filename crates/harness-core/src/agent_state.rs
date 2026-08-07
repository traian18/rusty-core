//! Durable mutable state owned by an agent.

use std::collections::{HashMap, VecDeque};

use harness_protocol::backend::ExecutionParams;
use harness_protocol::commands::{AgentError, AgentOperation, AgentStatus, UserInput};
use harness_protocol::ids::{AgentId, PermissionId, RunId, Timestamp, ToolCallId};
use harness_protocol::messages::AgentMessage;
use harness_protocol::tools::ToolCall;

use crate::context_state::AgentContextState;

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
    /// Session-level default model/execution parameters. Updated only via
    /// `AgentCommand::ConfigureExecution`; read by `execution_request()`
    /// when building each new run's `ExecutionRequest`.
    pub execution_params: ExecutionParams,
    /// Lossless canonical history. Compaction only changes the prepared inference view.
    pub messages: Vec<AgentMessage>,
    /// Per-agent inference-context checkpoint and pressure bookkeeping.
    pub context: AgentContextState,
    pub active_run: Option<RunId>,
    /// User inputs admitted while a run is active. Inputs are consumed in FIFO
    /// order once the active run reaches a terminal command boundary.
    pub queued_inputs: VecDeque<UserInput>,
    pub pending_tools: HashMap<ToolCallId, PendingToolCall>,
    /// Exact correlation from a permission request to its pending tool call.
    pub pending_permissions: HashMap<PermissionId, ToolCallId>,
    pub children: Vec<AgentId>,
    pub last_error: Option<AgentError>,
    /// Monotonic source for IDs and timestamps created by deterministic transitions.
    pub transition_sequence: u64,
    /// Nesting depth in the agent tree: zero for a root agent,
    /// parent depth plus one for a child. Read-only from the core's
    /// perspective; the runtime supervisor enforces depth limits.
    pub depth: u32,
}
