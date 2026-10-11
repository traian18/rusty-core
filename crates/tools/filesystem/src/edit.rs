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

/// Input for the `fs.edit` tool.
///
/// Two mutually exclusive modes:
/// - Whole-file replacement: set `content` only.
/// - Find-and-replace-once: set `old_text` and `new_text` only. `old_text`
///   must match exactly one location in the current file content — this is
///   deliberately strict (no partial/fuzzy matching, no "replace all") so a
///   caller can't silently edit the wrong occurrence or the wrong file.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct EditInput {
    /// Relative path of the file to edit.
    pub path: String,
    /// Whole-file replacement content. Mutually exclusive with
    /// `old_text`/`new_text`.
    #[serde(default)]
    pub content: Option<String>,
    /// Exact text to find in the current file and replace. Must appear
    /// exactly once. Requires `new_text`; mutually exclusive with `content`.
    #[serde(default)]
    pub old_text: Option<String>,
    /// Replacement text for `old_text`. Requires `old_text`.
    #[serde(default)]
    pub new_text: Option<String>,
}

/// Edits a file in the workspace, either by whole-file replacement or by a
/// unique find-and-replace patch.
pub struct EditTool {
    workspace: Arc<dyn Workspace>,
}

impl EditTool {
    pub fn new(workspace: Arc<dyn Workspace>) -> Self {
        Self { workspace }
    }
}

/// The two supported edit modes, resolved once from [`EditInput`] so
/// `execute` doesn't have to keep re-checking which fields are set.
enum EditMode {
    WholeFile { content: String },
    FindReplace { old_text: String, new_text: String },
}

fn resolve_mode(input: &EditInput) -> Result<EditMode, String> {
    match (&input.content, &input.old_text, &input.new_text) {
        (Some(content), None, None) => Ok(EditMode::WholeFile {
            content: content.clone(),
        }),
        (None, Some(old_text), Some(new_text)) => Ok(EditMode::FindReplace {
            old_text: old_text.clone(),
            new_text: new_text.clone(),
        }),
        (None, None, None) => {
            Err("one of `content` or (`old_text` and `new_text`) is required".to_string())
        }
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            Err("`content` is mutually exclusive with `old_text`/`new_text`".to_string())
        }
        (None, Some(_), None) => Err("`old_text` requires `new_text`".to_string()),
        (None, None, Some(_)) => Err("`new_text` requires `old_text`".to_string()),
    }
}

#[async_trait]
impl ToolExecutor for EditTool {
    fn descriptor(&self) -> ToolDescriptor {
        let schema = schemars::schema_for!(EditInput);
        ToolDescriptor {
            id: ToolId::new("fs.edit"),
            name: "Edit file".to_string(),
            description:
                "Replace the contents of a file in the workspace, either wholesale (`content`) \
                 or via a unique find-and-replace patch (`old_text`/`new_text`)"
                    .to_string(),
            input_schema: serde_json::to_value(schema).unwrap_or(json!({})),
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let input: EditInput = input.parse().map_err(|_| ToolError::ExecutionFailed)?;

        if cancel.is_cancelled() {
            return Err(ToolError::Timeout);
        }

        let mode = match resolve_mode(&input) {
            Ok(mode) => mode,
            Err(message) => {
                return Ok(ToolResult {
                    call_id: "fs.edit".to_string(),
                    output: json!({ "error": message }),
                    is_error: true,
                });
            }
        };

        let new_content = match mode {
            EditMode::WholeFile { content } => content,
            EditMode::FindReplace { old_text, new_text } => {
                info!(path = %input.path, "fs.edit: reading file for find-and-replace");
                let current = match self.workspace.read(&input.path).await {
                    Ok(current) => current,
                    Err(error) => {
                        info!(%error, "fs.edit: failed to read file for patch");
                        return Ok(ToolResult {
                            call_id: "fs.edit".to_string(),
                            output: json!({ "error": error.to_string() }),
                            is_error: true,
                        });
                    }
                };

                let occurrences = current.matches(old_text.as_str()).count();
                match occurrences {
                    0 => {
                        return Ok(ToolResult {
                            call_id: "fs.edit".to_string(),
                            output: json!({
                                "error": "old_text not found in the current file content"
                            }),
                            is_error: true,
                        });
                    }
                    1 => current.replacen(old_text.as_str(), &new_text, 1),
                    n => {
                        return Ok(ToolResult {
                            call_id: "fs.edit".to_string(),
                            output: json!({
                                "error": format!(
                                    "old_text is ambiguous: matched {n} locations, \
                                     provide more surrounding context to make it unique"
                                )
                            }),
                            is_error: true,
                        });
                    }
                }
            }
        };

        info!(path = %input.path, "fs.edit: writing file");
        match self.workspace.write(&input.path, &new_content).await {
            Ok(()) => Ok(ToolResult {
                call_id: "fs.edit".to_string(),
                output: json!({
                    "message": format!("Successfully wrote {} ({} bytes)", input.path, new_content.len())
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

#[cfg(test)]
mod tests;
