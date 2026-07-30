use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tracing::info;

use harness_tools::{CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult};
use harness_workspace::Workspace;

/// Input for the `fs.read` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ReadInput {
    /// Relative path of the file to read.
    pub path: String,
}

/// Reads a file from the workspace and returns its contents as text.
pub struct ReadTool {
    workspace: Arc<dyn Workspace>,
}

impl ReadTool {
    pub fn new(workspace: Arc<dyn Workspace>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl ToolExecutor for ReadTool {
    fn descriptor(&self) -> ToolDescriptor {
        let schema = schemars::schema_for!(ReadInput);
        ToolDescriptor {
            id: ToolId::new("fs.read"),
            name: "Read file".to_string(),
            description: "Read the contents of a file in the workspace".to_string(),
            input_schema: serde_json::to_value(schema).unwrap_or(json!({})),
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let input: ReadInput = input.parse()
            .map_err(|_| ToolError::ExecutionFailed)?;

        if cancel.is_cancelled() {
            return Err(ToolError::Timeout);
        }

        info!(path = %input.path, "fs.read: reading file");

        match self.workspace.read(&input.path).await {
            Ok(content) => Ok(ToolResult {
                call_id: "fs.read".to_string(),
                output: json!({ "content": content }),
                is_error: false,
            }),
            Err(e) => {
                info!(error = %e, "fs.read: failed to read file");
                Ok(ToolResult {
                    call_id: "fs.read".to_string(),
                    output: json!({ "error": e.to_string() }),
                    is_error: true,
                })
            }
        }
    }
}
