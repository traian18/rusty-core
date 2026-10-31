//! The reviewer session that judges a finished build.

use serde_json::Value;

use super::verdict::{result_schema, validate};
use crate::orchestration::{AgentExecutionError, AgentStepRequest};
use crate::task_queue::queue::Queue;
use crate::task_queue::scope::{is_read_only, REVIEW_SCOPE};

impl Queue<'_> {
    /// Has the reviewer judge `built`. `host_checks` is what the workflow's own typecheck found.
    ///
    /// The reviewer never gets a mutating tool, even if a user supplies a permissive profile.
    /// Checks run in the dedicated validation step.
    pub(super) async fn review(
        &mut self,
        mut child: AgentStepRequest,
        built: Value,
        host_checks: Option<String>,
    ) -> Result<Value, AgentExecutionError> {
        child.profile = Some(self.settings.review_profile.clone());
        child.instructions = format!("{}\n\n{REVIEW_SCOPE}", self.settings.review_instructions);
        child.output_schema = result_schema(true);
        child.input["build"] = built;
        if let Some(note) = host_checks {
            child.input["host_checks"] = Value::String(note);
        }
        child.tools.retain(|tool| is_read_only(tool));
        let schema = child.output_schema.clone();
        let reviewed = self
            .executor
            .run_single_agent_step(child, self.context, self.usage)
            .await?
            .value;
        validate(&schema, &reviewed, "invalid_task_review")?;
        Ok(reviewed)
    }
}
