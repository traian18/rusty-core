//! Walking the plan: each task in order, then the repair task, with the failure policy in between.

use serde_json::json;

use super::model::{AfterFailure, Task, TaskOutcome, DESIGN_GAP};
use super::queue::Queue;
use crate::orchestration::{AgentExecutionError, CHANGES_REQUESTED};

impl Queue<'_> {
    /// Works through the planned tasks that are not done yet, then the repair task if one is due.
    pub(super) async fn work_through(&mut self) -> Result<(), AgentExecutionError> {
        let plan = self.plan;
        let repair = self.repair_task();
        let mut questions = 0;
        for task in &plan.tasks[self.progress.completed.len()..] {
            self.work_on(task, true, &mut questions).await?;
        }
        if let Some(task) = repair {
            self.work_on(&task, false, &mut questions).await?;
            self.progress.pending_repair = None;
            self.checkpoint().await?;
        }
        Ok(())
    }

    /// The task that makes the changes the verification or the user asked for after the plan.
    fn repair_task(&self) -> Option<Task> {
        let instructions = self.progress.pending_repair.clone()?;
        Some(Task {
            id: "final-repair".into(),
            requirement_ids: Vec::new(),
            criterion_ids: Vec::new(),
            depends_on: Vec::new(),
            flow: None,
            instructions,
        })
    }

    /// Does `task` until it is accepted or the failure policy skips it. A planned task is
    /// recorded in the progress; the repair task is not.
    async fn work_on(
        &mut self,
        task: &Task,
        planned: bool,
        questions: &mut usize,
    ) -> Result<(), AgentExecutionError> {
        if planned && self.skip_if_blocked(task).await? {
            return Ok(());
        }
        let mut guidance: Option<String> = None;
        loop {
            let reason = match self.run_task(task, guidance.as_deref(), planned).await? {
                TaskOutcome::Accepted => return Ok(()),
                TaskOutcome::Failed(reason) => reason,
            };
            *questions += 1;
            match self.after_failure(task, &reason, *questions).await? {
                AfterFailure::Retry(notes) => guidance = notes,
                AfterFailure::Skip => return self.skip(task, &reason, planned).await,
                AfterFailure::RevisePlan(changes) => {
                    return Err(AgentExecutionError::retryable(
                        CHANGES_REQUESTED,
                        changes,
                        rusty_workflow_core::RetryReason::ChangesRequested,
                    ));
                }
            }
        }
    }

    /// A task built on a skipped one would build on a gap: skip it too. True when it was skipped.
    async fn skip_if_blocked(&mut self, task: &Task) -> Result<bool, AgentExecutionError> {
        let Some(gap) = self.progress.skipped_dependency(task) else {
            return Ok(false);
        };
        self.progress.completed.push(json!({
            "task_id": task.id,
            "skipped": true,
            "reason": format!("Task {} builds on {gap}, which was skipped, so it was not built.", task.id),
        }));
        self.checkpoint().await?;
        Ok(true)
    }

    async fn skip(
        &mut self,
        task: &Task,
        reason: &str,
        planned: bool,
    ) -> Result<(), AgentExecutionError> {
        if planned {
            let reason = reason
                .strip_prefix(DESIGN_GAP)
                .map_or(reason, str::trim_start);
            self.progress
                .completed
                .push(json!({"task_id": task.id, "skipped": true, "reason": reason}));
        } else {
            self.progress.pending_repair = None;
        }
        self.progress.current = None;
        self.progress.step_plan = None;
        self.checkpoint().await
    }
}
