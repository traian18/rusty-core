use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tracing::info;

use harness_tools::{CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult};
use harness_workspace::Workspace;

/// Input for the `fs.edit` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct EditInput {
    /// Relative path of the file to write.
    pub path: String,
    /// The new full content of the file (whole-file replacement, not a diff).
    pub content: String,
}

/// Replaces an entire file in the workspace with new content.
///
/// Phase 4 uses whole-file replacement. Patch/diff support is deferred to a
/// later phase to keep the tool surface minimal.
pub struct EditTool {
    workspace: Arc<dyn Workspace>,
}

impl EditTool {
    pub fn new(workspace: Arc<dyn Workspace>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl ToolExecutor for EditTool {
    fn descriptor(&self) -> ToolDescriptor {
        let schema = schemars::schema_for!(EditInput);
        ToolDescriptor {
            id: ToolId::new("fs.edit"),
            name: "Edit file".to_string(),
            description: "Replace the contents of a file in the workspace".to_string(),
            input_schema: serde_json::to_value(schema).unwrap_or(json!({})),
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let input: EditInput = input.parse()
            .map_err(|_| ToolError::ExecutionFailed)?;

        if cancel.is_cancelled() {
            return Err(ToolError::Timeout);
        }

        info!(path = %input.path, "fs.edit: writing file");

        match self.workspace.write(&input.path, &input.content).await {
            Ok(()) => Ok(ToolResult {
                call_id: "fs.edit".to_string(),
                output: json!({
                    "message": format!("Successfully wrote {} ({} bytes)", input.path, input.content.len())
                }),
                is_error: false,
            }),
            Err(e) => {
                info!(error = %e, "fs.edit: failed to write file");
                Ok(ToolResult {
                    call_id: "fs.edit".to_string(),
                    output: json!({ "error": e.to_string() }),
                    is_error: true,
                })
            }
        }
    }
}
