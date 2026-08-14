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

pub const SKILL_READ: &str = "skill.read";

/// Input for the `skill.read` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ReadInput {
    /// Name of the skill that bundles the file.
    pub skill: String,
    /// Path of the file, relative to the skill's own directory, as listed
    /// by `skill.load`.
    pub path: String,
}

/// Reads a file bundled inside one skill's directory.
///
/// Deliberately separate from `fs.read`: skill directories — especially the
/// user-level `$HOME/.harness/skills` — sit outside the workspace root,
/// where `FsWorkspace`'s traversal guard correctly refuses to read. Rather
/// than widen that guard, this tool grants a second, much narrower scope:
/// one skill's own directory, enforced by `Skill::read_bundled`.
pub struct SkillReadTool {
    catalog: Arc<SkillCatalog>,
}

impl SkillReadTool {
    pub fn new(catalog: Arc<SkillCatalog>) -> Self {
        Self { catalog }
    }
}

#[async_trait]
impl ToolExecutor for SkillReadTool {
    fn descriptor(&self) -> ToolDescriptor {
        let schema = schemars::schema_for!(ReadInput);
        ToolDescriptor {
            id: ToolId::new(SKILL_READ),
            name: "Read skill file".to_string(),
            description: "Read a file bundled with a skill, using a path relative to that skill's \
                 directory as reported by skill.load."
                .to_string(),
            input_schema: serde_json::to_value(schema).unwrap_or(json!({})),
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let input: ReadInput = input.parse().map_err(|_| ToolError::ExecutionFailed)?;

        if cancel.is_cancelled() {
            return Err(ToolError::Timeout);
        }

        info!(skill = %input.skill, path = %input.path, "skill.read: reading bundled file");

        let Some(skill) = self.catalog.get(&input.skill) else {
            return Ok(error_result(format!("no skill named {:?}", input.skill)));
        };

        match skill.read_bundled(&input.path).await {
            Ok(content) => Ok(ToolResult {
                call_id: SKILL_READ.to_string(),
                output: json!({ "content": content }),
                is_error: false,
            }),
            // Includes the refusals from `read_bundled`'s scoping checks. A
            // rejected path is a logical error the model can see and correct,
            // not an infrastructure fault that should abort the run.
            Err(error) => Ok(error_result(error.to_string())),
        }
    }
}

fn error_result(message: String) -> ToolResult {
    ToolResult {
        call_id: SKILL_READ.to_string(),
        output: json!({ "error": message }),
        is_error: true,
    }
}
