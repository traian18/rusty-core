use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tracing::info;

use harness_tools::{
    CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult,
};
use harness_workspace::Workspace;

/// Input for the `workspace.search` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchInput {
    /// The search query (case-insensitive substring match).
    pub query: String,
}

/// Searches all text files in the workspace for the given query.
///
/// Delegates to [`Workspace::search`] and formats the results into a
/// human-readable `ToolResult`.
pub struct SearchTool {
    workspace: Arc<dyn Workspace>,
}

impl SearchTool {
    pub fn new(workspace: Arc<dyn Workspace>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl ToolExecutor for SearchTool {
    fn descriptor(&self) -> ToolDescriptor {
        let schema = schemars::schema_for!(SearchInput);
        ToolDescriptor {
            id: ToolId::new("workspace.search"),
            name: "Search workspace".to_string(),
            description: "Search all text files in the workspace for a query string".to_string(),
            input_schema: serde_json::to_value(schema).unwrap_or(json!({})),
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let input: SearchInput = input.parse().map_err(|_| ToolError::ExecutionFailed)?;

        if cancel.is_cancelled() {
            return Err(ToolError::Timeout);
        }

        info!(query = %input.query, "workspace.search: searching");

        match self.workspace.search(&input.query).await {
            Ok(result) => {
                let content = if result.matches.is_empty() {
                    format!("No matches found for query '{}'", input.query)
                } else {
                    let mut lines = vec![format!(
                        "{} matches found for '{}'",
                        result.total_count, input.query
                    )];
                    for m in &result.matches {
                        let rel = m.file_path.display();
                        lines.push(format!("  {}:{}: {}", rel, m.line_number, m.line_content));
                    }
                    lines.join("\n")
                };
                Ok(ToolResult {
                    call_id: "workspace.search".to_string(),
                    output: json!({ "results": content, "match_count": result.total_count }),
                    is_error: false,
                })
            }
            Err(e) => {
                info!(error = %e, "workspace.search: failed to search");
                Ok(ToolResult {
                    call_id: "workspace.search".to_string(),
                    output: json!({ "error": e.to_string() }),
                    is_error: true,
                })
            }
        }
    }
}
