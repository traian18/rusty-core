//! Building a task once, watched: the whole task, or, when it was split, its remaining steps.

use serde_json::json;

use crate::orchestration::{AgentExecutionError, AgentStepRequest};
use crate::task_queue::model::Task;
use crate::task_queue::queue::Queue;
use crate::task_queue::recovery::Built;
use crate::task_queue::split;

impl Queue<'_> {
    /// Builds a task once. A split task's remaining steps run one after the other, each a watched
    /// build that must leave the code compiling.
    pub(super) async fn build_task(
        &mut self,
        child: &AgentStepRequest,
        task: &Task,
    ) -> Result<Built, AgentExecutionError> {
        let Some((steps, done)) = self.progress.pending_split(task) else {
            return self
                .executor
                .build_watched(child.clone(), self.context, self.usage)
                .await;
        };
        let mut summaries = Vec::new();
        for index in done..steps.len() {
            let place = format!("Step {} of {}", index + 1, steps.len());
            let mut step = child.clone();
            step.instructions
                .push_str(&split::step_instructions(&steps, index));
            let reply = match self
                .executor
                .build_watched(step, self.context, self.usage)
                .await?
            {
                Built::Stopped(reason) => return Ok(Built::Stopped(format!("{place}: {reason}"))),
                Built::Replied(reply) => reply,
            };
            let summary = reply["summary"].as_str().unwrap_or("no summary");
            let status = reply["status"].as_str().unwrap_or("invalid");
            if status != "complete" && status != "blocked_environment" {
                let stopped = json!({"status": status, "summary": format!("{place}: {summary}")});
                return Ok(Built::Replied(stopped));
            }
            summaries.push(format!("{place}: {summary}"));
            self.finish_step(task, index).await?;
        }
        Ok(Built::Replied(json!({
            "status": "complete",
            "summary": format!("Built in {} steps.\n{}", steps.len(), summaries.join("\n")),
        })))
    }

    /// Counts the step as done and makes the tree after it the baseline, so a later stop undoes
    /// only the step that got stuck.
    async fn finish_step(&mut self, task: &Task, index: usize) -> Result<(), AgentExecutionError> {
        if let Some(split) = self.progress.split.as_mut() {
            split["done"] = json!(index + 1);
        }
        let (executor, request) = (self.executor, self.request);
        executor
            .advance_baseline(
                request,
                task,
                &mut self.progress,
                &format!("step-{}", index + 1),
            )
            .await;
        self.checkpoint().await
    }
}
