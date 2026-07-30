//! Canonical tool execution traits and shared data types.
//!
//! Concrete tools live in dedicated crates such as
//! `harness-tool-filesystem` and `harness-tool-shell`. Keeping implementations
//! out of this crate prevents placeholder executors from being registered in
//! production by mistake.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Re-export `CancellationToken` from tokio-util so consumers do not need a
/// direct dependency on that crate.
pub use tokio_util::sync::CancellationToken;

/// Unique identifier for a tool within the harness-tools type system.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ToolId(pub String);

impl ToolId {
    /// Create a new `ToolId` from a string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Return the inner string as a `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ToolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<String> for ToolId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ToolId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// Describes a tool and its model-facing input schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolDescriptor {
    /// Stable machine-readable identifier, such as `fs.read`.
    pub id: ToolId,
    /// Human-readable display name.
    pub name: String,
    /// Description of the operation.
    pub description: String,
    /// JSON Schema for the tool arguments.
    pub input_schema: serde_json::Value,
}

/// Raw JSON arguments supplied to a tool executor.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolInput {
    pub arguments: serde_json::Value,
}

impl ToolInput {
    /// Deserialize the arguments into a concrete input type.
    pub fn parse<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(self.arguments.clone())
    }
}

/// Successful or logical-error output produced by a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Executor-local correlation value. The runtime replaces this with the
    /// protocol call ID before publishing the result.
    pub call_id: String,
    pub output: serde_json::Value,
    pub is_error: bool,
}

/// Infrastructure errors that prevent a tool from producing a result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolError {
    ExecutionFailed,
    PermissionDenied,
    Timeout,
    Internal,
}

/// Structured failure retained for compatibility with progress/reporting APIs.
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

/// The contract implemented by every concrete tool.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    fn descriptor(&self) -> ToolDescriptor;

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError>;
}

/// Fallback executor used for an unrecognized configured descriptor.
pub struct UnknownTool {
    pub descriptor: ToolDescriptor,
}

#[async_trait]
impl ToolExecutor for UnknownTool {
    fn descriptor(&self) -> ToolDescriptor {
        self.descriptor.clone()
    }

    async fn execute(
        &self,
        _input: ToolInput,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        Err(ToolError::ExecutionFailed)
    }
}
