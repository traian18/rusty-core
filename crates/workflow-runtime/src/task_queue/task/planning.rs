//! Planning a task in detail before it is built, and splitting it when it is too big.

use rusty_workflow_core::StructuredOutputMode;
use serde_json::{json, Value};

use crate::orchestration::{is_user_change, AgentExecutionError};
use crate::task_queue::model::Task;
use crate::task_queue::queue::Queue;
use crate::task_queue::scope::is_read_only;
use crate::task_queue::{brief, planner_watchdog, split};

/// A step plan (or none), or the question a decision only the person can make comes down to.
pub(super) type Planned = Result<Option<String>, String>;

impl Queue<'_> {
    /// The task planner's detailed plan for `task`, when the queue has a planner: the one already
    /// made for it (kept across repairs and a resumed run), or a new one. The user's guidance for
    /// another try asks for a new plan that takes it into account. A planner that finds the task
    /// too big splits it here, before any build (no plan: the steps are the plan); one that finds
    /// a decision only the person can make returns its question as the inner `Err`.
    pub(super) async fn plan_task(
        &mut self,
        task: &Task,
        guidance: Option<&str>,
    ) -> Result<Planned, AgentExecutionError> {
        if self.settings.task_planner.is_none() {
            return Ok(Ok(None));
        }
        // A split task's steps are its plan: a new plan of the whole task would contradict "do
        // only this step".
        if self.progress.pending_split(task).is_some() {
            return Ok(Ok(None));
        }
        if let Some(kept) = self.kept_step_plan(task, guidance) {
            return Ok(Ok(Some(kept)));
        }
        let reply = self.ask_planner(task, guidance).await?;
        let planned = match split::read_planned(&reply) {
            split::Planned::Gap(question) => return Ok(Err(question)),
            split::Planned::Split(steps) => {
                split::record_split(&mut self.progress, task, &steps);
                None
            }
            split::Planned::Plan(plan) => {
                self.progress.step_plan = Some(json!({"task_id": task.id, "plan": plan}));
                Some(plan)
            }
        };
        self.checkpoint().await?;
        Ok(Ok(planned))
    }

    fn kept_step_plan(&self, task: &Task, guidance: Option<&str>) -> Option<String> {
        self.progress
            .step_plan
            .as_ref()
            .filter(|kept| kept["task_id"] == task.id.as_str() && guidance.is_none())
            .and_then(|kept| kept["plan"].as_str())
            .map(str::to_owned)
    }

    async fn ask_planner(
        &mut self,
        task: &Task,
        guidance: Option<&str>,
    ) -> Result<String, AgentExecutionError> {
        let Some(planner) = &self.settings.task_planner else {
            return Ok(String::new());
        };
        let mut child = self.request.clone();
        child.task_queue = None;
        child.checkpoint = None;
        child
            .feedback
            .retain(|feedback| !is_user_change(&feedback.code));
        child.profile = Some(planner.profile.clone());
        child.instructions = format!(
            "{}\n\n{}\n\n{}",
            planner.instructions,
            split::FIT_INSTRUCTIONS,
            split::MATERIAL_INSTRUCTIONS
        );
        if let Some(guidance) = guidance {
            child.instructions.push_str(&format!(
                "\n\nThe task failed before, and the user asked for another try with this guidance; plan for it:\n{guidance}"
            ));
        }
        child.structured_output = StructuredOutputMode::Text;
        child.output_schema = json!({"type": "string"});
        child.input = brief::planner_input(self.request, self.plan, task, &self.progress);
        child.tools.retain(|tool| is_read_only(tool));
        let planned = self
            .executor
            .run_watched_agent_step(child, self.context, self.usage, planner_watchdog())
            .await?
            .value;
        let text = match planned {
            Value::String(text) => text,
            other => crate::markdown_reply::render_input(&other),
        };
        if text.trim().is_empty() {
            return Err(AgentExecutionError::new(
                "invalid_task_result",
                format!("the planner returned no plan for task {}", task.id),
            ));
        }
        Ok(text)
    }

    /// Tries to split a stuck task into smaller steps, once per task. True when it was split.
    pub(super) async fn try_split(&mut self, task: &Task) -> Result<bool, AgentExecutionError> {
        if self.progress.split_tried(task) {
            return Ok(false);
        }
        let steps = self
            .executor
            .split_task(
                self.request,
                self.plan,
                task,
                &mut self.progress,
                self.context,
                self.usage,
            )
            .await?;
        Ok(steps.is_some())
    }
}
