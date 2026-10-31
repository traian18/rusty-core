//! What goes into a tool call and what comes back: the descriptor the model sees, the arguments,
//! the result, and the infrastructure errors that prevent a result.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::tool_id::ToolId;

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
