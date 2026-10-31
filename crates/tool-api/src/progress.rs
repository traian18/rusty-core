//! Reporting types kept for the progress and reporting APIs of tool hosts.

use serde::Serialize;

use crate::tool_id::ToolId;

/// Why a tool call failed, in the shape reporting hosts expect.
#[derive(Debug, Clone, Serialize)]
pub struct ExecutionFailure {
    pub kind: FailureKind,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub enum FailureKind {
    InvalidInput,
    WorkspaceError,
    ExecutionError,
    Timeout,
    Cancelled,
}

/// Structured execution output retained for compatibility with reporting APIs.
#[derive(Debug, Clone, Serialize)]
pub struct ExecutionResult {
    pub success: bool,
    pub content: String,
    pub failure: Option<ExecutionFailure>,
    pub usage: Option<ToolUsage>,
}

impl ExecutionResult {
    pub fn cancelled() -> Self {
        Self {
            success: false,
            content: String::new(),
            failure: Some(ExecutionFailure {
                kind: FailureKind::Cancelled,
                message: "tool call was cancelled".to_string(),
            }),
            usage: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ToolUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolProgress {
    pub tool_call_id: ToolId,
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
