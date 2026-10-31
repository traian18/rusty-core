//! Doing one task: plan it, build it under a watchdog, typecheck it, have it reviewed.
//!
//! A task is tried in rounds of up to `max_repairs + 1` attempts. A round that ends without an
//! accepted task is either kept for the user to decide on (the build is finished but the review
//! keeps sending it back), or undone and, once per task, retried in smaller steps.

mod attempt;
mod build;
mod builder;
mod explain;
mod flow;
mod planning;
mod review;
pub(super) mod verdict;

use serde_json::Value;

use super::model::{Ended, Task, TaskOutcome};
use super::queue::Queue;
use crate::orchestration::AgentExecutionError;

/// What is being tried: the task, the user's guidance for this try, and the detailed plan.
pub(super) struct TaskRun<'t> {
    pub(super) task: &'t Task,
    pub(super) guidance: Option<&'t str>,
    pub(super) step_plan: Option<&'t str>,
    /// Whether an accepted task is recorded as completed (the repair task is not).
    pub(super) record: bool,
}

/// How a round of attempts ended.
enum Rounds {
    Settled(TaskOutcome),
    Ended(Ended),
}

impl Queue<'_> {
    /// Builds and reviews one task, repairing up to `max_repairs` times, and records it as
    /// completed when it is a planned task (`record`). A task that cannot be completed is
    /// reported, not raised: what happens next is the queue's failure policy.
    pub(in crate::task_queue) async fn run_task(
        &mut self,
        task: &Task,
        guidance: Option<&str>,
        record: bool,
    ) -> Result<TaskOutcome, AgentExecutionError> {
        if let Some(target) = self.flow_target(task) {
            return self.run_task_flow(task, &target, record).await;
        }
        self.start_from_the_baseline(task, guidance).await?;
        let mut step_plan = match self.plan_task(task, guidance).await? {
            Ok(step_plan) => step_plan,
            // Nothing was built: the person answers, and another try plans with the answer.
            Err(question) => return Ok(TaskOutcome::Failed(explain::design_gap(task, &question))),
        };
        let cancel = self.context.cancellation.clone();
        self.executor
            .check_before_build(task, step_plan.as_deref(), &mut self.progress, &cancel)
            .await;
        loop {
            let job = TaskRun {
                task,
                guidance,
                step_plan: step_plan.as_deref(),
                record,
            };
            let ended = match self.rounds(&job).await? {
                Rounds::Settled(outcome) => return Ok(outcome),
                Rounds::Ended(ended) => ended,
            };
            if let Some(why) = explain::kept(&ended, self.settings.max_repairs, &self.progress) {
                return self.keep_the_work(task, &why).await;
            }
            let why = explain::undone(&ended, self.settings.max_repairs);
            self.undo_and_checkpoint(task, &why).await?;
            if self.try_split(task).await? {
                step_plan = None;
                continue;
            }
            return Ok(TaskOutcome::Failed(explain::not_completed(task, &why)));
        }
    }

    /// A task that is tried again starts from the tree it started from, not from the unfinished
    /// edits of its last attempt.
    async fn start_from_the_baseline(
        &mut self,
        task: &Task,
        guidance: Option<&str>,
    ) -> Result<(), AgentExecutionError> {
        let (executor, request) = (self.executor, self.request);
        executor.baseline(request, task, &mut self.progress).await;
        if super::recovery::needs_undo(&self.progress, task, guidance) {
            executor
                .undo_attempt(request, task, &mut self.progress, "the task is tried again")
                .await;
        }
        self.checkpoint().await
    }

    async fn rounds(&mut self, job: &TaskRun<'_>) -> Result<Rounds, AgentExecutionError> {
        let mut last = Ended::Unfinished;
        for repair in 0..=self.settings.max_repairs {
            if self.context.cancellation.is_cancelled() {
                return Err(AgentExecutionError::new(
                    "cancelled",
                    "task queue cancelled",
                ));
            }
            let after_rejection = matches!(last, Ended::Rejected);
            match self.attempt(job, repair, after_rejection).await? {
                attempt::Attempt::Finished(outcome) => return Ok(Rounds::Settled(outcome)),
                attempt::Attempt::Stop(ended) => return Ok(Rounds::Ended(ended)),
                attempt::Attempt::Retry(ended) => last = ended,
            }
        }
        Ok(Rounds::Ended(last))
    }

    /// A finished build the review keeps sending back is kept: undoing and splitting it cannot
    /// settle what the review wants. The user decides, and another try builds on it.
    async fn keep_the_work(
        &mut self,
        task: &Task,
        why: &str,
    ) -> Result<TaskOutcome, AgentExecutionError> {
        self.progress.annotate_current("kept", Value::Bool(true));
        self.checkpoint().await?;
        Ok(TaskOutcome::Failed(explain::kept_work(task, why)))
    }

    /// The attempt got stuck, did not finish, or kept adding errors: undo it.
    async fn undo_and_checkpoint(
        &mut self,
        task: &Task,
        why: &str,
    ) -> Result<(), AgentExecutionError> {
        let (executor, request) = (self.executor, self.request);
        executor
            .undo_attempt(request, task, &mut self.progress, why)
            .await;
        self.checkpoint().await
    }
}
