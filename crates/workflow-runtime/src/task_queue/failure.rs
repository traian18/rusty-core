//! The queue's failure policy: stop, skip, or ask the user what to do.

use rusty_workflow_core::{InputDecision, InputRequest, TaskFailurePolicy};
use serde_json::json;

use super::model::{AfterFailure, Task, DESIGN_GAP};
use super::queue::Queue;
use crate::orchestration::AgentExecutionError;

impl Queue<'_> {
    /// What to do about a task that could not be completed. `question` numbers the asks of this
    /// run, so each gets its own id.
    pub(super) async fn after_failure(
        &mut self,
        task: &Task,
        reason: &str,
        question: usize,
    ) -> Result<AfterFailure, AgentExecutionError> {
        let design_gap = reason.starts_with(DESIGN_GAP);
        let reason = reason
            .strip_prefix(DESIGN_GAP)
            .map_or(reason, str::trim_start);
        match self.settings.on_task_failure {
            TaskFailurePolicy::Stop => Err(AgentExecutionError::new("task_failed", reason)),
            TaskFailurePolicy::Skip => Ok(AfterFailure::Skip),
            TaskFailurePolicy::Ask => {
                self.ask_what_to_do(task, reason, design_gap, question)
                    .await
            }
        }
    }

    async fn ask_what_to_do(
        &mut self,
        task: &Task,
        reason: &str,
        design_gap: bool,
        question: usize,
    ) -> Result<AfterFailure, AgentExecutionError> {
        let revisable = self.request.plan_revisable;
        let request = InputRequest {
            id: format!(
                "{}:{}:{}:{question}",
                self.request.node_id, self.request.attempt, task.id
            ),
            kind: "task_failure".into(),
            prompt: format!(
                "Task {} could not be completed. Try it again (your notes guide the next try), skip it, {}or stop the workflow.",
                task.id,
                if revisable { "revise the plan, " } else { "" }
            ),
            // A design gap is not a failed build: skipping it skips every task built on it, so
            // the host is told it is one (Auto mode revises the plan instead of skipping).
            subject: json!({
                "task": format!("{}: {}", task.id, task.instructions),
                "problem": reason,
                "design_gap": design_gap,
            }),
            decisions: decisions(revisable),
        };
        let response = self.context.ask(request).await?;
        let notes = response.text.filter(|text| !text.trim().is_empty());
        match response.decision.as_str() {
            "retry" => Ok(AfterFailure::Retry(notes)),
            "skip" => Ok(AfterFailure::Skip),
            "revise_plan" => Ok(AfterFailure::RevisePlan(notes.unwrap_or_default())),
            _ => Err(AgentExecutionError::new(
                "task_stopped",
                format!("{reason}. Stopped at your request; completed tasks are checkpointed."),
            )),
        }
    }
}

fn decisions(revisable: bool) -> Vec<InputDecision> {
    let decision = |id: &str, label: &str, requires_text: bool| InputDecision {
        id: id.into(),
        label: label.into(),
        requires_text,
    };
    let mut decisions = vec![
        decision("retry", "Try again", false),
        decision("skip", "Skip this task", false),
    ];
    if revisable {
        decisions.push(decision("revise_plan", "Revise the plan", true));
    }
    decisions.push(decision("stop", "Stop", false));
    decisions
}
