//! Progress and result shapes tools report through the workspace layer.

use serde::Serialize;

/// Progress event emitted during long-running tool operations.
#[derive(Debug, Clone, Serialize)]
pub struct ToolProgress {
    pub tool_call_id: String,
    pub phase: ProgressPhase,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub enum ProgressPhase {
    Started,
    Streaming,
    Completed,
    Failed,
}

/// Result of a tool execution.
#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub success: bool,
    pub content: String,
}
