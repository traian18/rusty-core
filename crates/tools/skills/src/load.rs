use std::sync::Arc;

use async_trait::async_trait;
use harness_skills::SkillCatalog;
use harness_tools::{
    CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tracing::info;

pub const SKILL_LOAD: &str = "skill.load";

/// Input for the `skill.load` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct LoadInput {
    /// Name of the skill to load, exactly as it appears in the skill
    /// catalog in the system prompt.
    pub name: String,
}

/// Returns one skill's full instructions plus a listing of the files it
/// bundles.
///
/// This is the second half of progressive disclosure: the system prompt
/// advertises only names and descriptions, and this tool is how the model
/// pays for the body of the one skill it actually needs.
pub struct SkillLoadTool {
    catalog: Arc<SkillCatalog>,
}

impl SkillLoadTool {
    pub fn new(catalog: Arc<SkillCatalog>) -> Self {
        Self { catalog }
    }
}

#[async_trait]
impl ToolExecutor for SkillLoadTool {
    fn descriptor(&self) -> ToolDescriptor {
        let schema = schemars::schema_for!(LoadInput);
        ToolDescriptor {
            id: ToolId::new(SKILL_LOAD),
            name: "Load skill".to_string(),
            description: "Read a skill's full instructions and the list of files it bundles. \
                 Call this before acting on a skill listed in the skill catalog."
                .to_string(),
            input_schema: serde_json::to_value(schema).unwrap_or(json!({})),
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let input: LoadInput = input.parse().map_err(|_| ToolError::ExecutionFailed)?;

        if cancel.is_cancelled() {
            return Err(ToolError::Timeout);
        }

        info!(skill = %input.name, "skill.load: loading skill");

        // An unknown name is the model's mistake, not an infrastructure
        // fault, so it comes back as a logical error the model can read and
        // recover from — the same stance `fs.read` and `McpToolExecutor`
        // take. Listing what *is* available turns a dead end into a retry.
        let Some(skill) = self.catalog.get(&input.name) else {
            let available: Vec<&str> = self
                .catalog
                .iter()
                .map(|skill| skill.name.as_str())
                .collect();
            return Ok(error_result(format!(
                "no skill named {:?}. Available skills: {}",
                input.name,
                if available.is_empty() {
                    "(none)".to_string()
                } else {
                    available.join(", ")
                }
            )));
        };

        let files = match skill.bundled_files().await {
            Ok(files) => files,
            // The instructions are the valuable part and we already have
            // them; a directory listing that failed shouldn't sink the call.
            Err(error) => {
                info!(skill = %input.name, error = %error, "skill.load: could not list bundled files");
                Vec::new()
            }
        };

        Ok(ToolResult {
            call_id: SKILL_LOAD.to_string(),
            output: json!({
                "name": skill.name,
                "description": skill.description,
                "instructions": skill.instructions,
                "files": files,
                "allowed_tools": skill.allowed_tools,
            }),
            is_error: false,
        })
    }
}

fn error_result(message: String) -> ToolResult {
    ToolResult {
        call_id: SKILL_LOAD.to_string(),
        output: json!({ "error": message }),
        is_error: true,
    }
}
