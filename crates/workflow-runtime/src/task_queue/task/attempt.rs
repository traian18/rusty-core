//! One attempt at a task: build, typecheck, review.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::verdict::{review_complete, validate};
use super::{explain, TaskRun};
use crate::orchestration::AgentExecutionError;
use crate::task_queue::checks::CheckErrors;
use crate::task_queue::model::{Ended, Task, TaskOutcome};
use crate::task_queue::queue::Queue;
use crate::task_queue::recovery::Built;

/// How an attempt ended.
pub(super) enum Attempt {
    /// The task is settled: accepted, or failed in a way another attempt cannot change.
    Finished(TaskOutcome),
    /// The round is over: another attempt in it cannot help.
    Stop(Ended),
    /// Try again in the same round.
    Retry(Ended),
}

fn builder_done(status: &str) -> bool {
    status == "complete" || status == "blocked_environment"
}

impl Queue<'_> {
    pub(super) async fn attempt(
        &mut self,
        job: &TaskRun<'_>,
        repair: u32,
        after_rejection: bool,
    ) -> Result<Attempt, AgentExecutionError> {
        let task = job.task;
        let child = self.builder_request(job);
        let before = self.executor.tree_fingerprint().await;
        let built = match self.build_task(&child, task).await? {
            Built::Replied(reply) => reply,
            Built::Stopped(reason) => {
                self.progress.current =
                    Some(json!({"task_id": task.id, "stopped": reason, "repair": repair}));
                return Ok(Attempt::Stop(Ended::Stopped(reason)));
            }
        };
        validate(&child.output_schema, &built, "invalid_task_result")?;
        let status = built["status"].as_str().unwrap_or("invalid").to_owned();
        self.record_build(task, &built, &status, &before, repair)
            .await?;
        if status == "checkpoint" || status == "needs_repair" {
            return Ok(Attempt::Retry(Ended::Unfinished));
        }
        // A check that cannot run here (no network, a missing tool) does not stop the work: the
        // reviewer judges the code that was written, and the final validation reports what did
        // not run.
        if !builder_done(&status) {
            let failed = explain::builder_stopped(task, &status, &built);
            return Ok(Attempt::Finished(TaskOutcome::Failed(failed)));
        }
        // The workflow's own typecheck: errors the build added go back to the builder before any
        // review.
        let cancel = self.context.cancellation.clone();
        let checked = self
            .executor
            .check_after_build(&mut self.progress, &cancel)
            .await;
        if !checked.added.is_empty() {
            self.progress
                .annotate_current("new_errors", json!(checked.added));
            self.checkpoint().await?;
            return Ok(Attempt::Retry(Ended::NewErrors));
        }
        // Without a git repository nothing can be compared: count it as changed.
        let changed = before.is_empty() || self.executor.tree_fingerprint().await != before;
        let reviewed = self.review(child, built.clone(), checked.note).await?;
        self.progress.annotate_current("review", reviewed.clone());
        self.judge(
            job,
            built,
            reviewed,
            checked.results,
            (changed, after_rejection),
        )
        .await
    }

    /// Notes the build in the progress. A build that stops before changing anything was not a try:
    /// the next attempt is told so, instead of inheriting its reasons to stop.
    async fn record_build(
        &mut self,
        task: &Task,
        built: &Value,
        status: &str,
        before: &[(String, String)],
        repair: u32,
    ) -> Result<(), AgentExecutionError> {
        let unfinished = status == "checkpoint" || status == "needs_repair";
        let unchanged =
            unfinished && !before.is_empty() && self.executor.tree_fingerprint().await == before;
        self.progress.current = Some(json!({"task_id": task.id, "build": built, "repair": repair}));
        if unchanged {
            self.progress
                .annotate_current("unchanged", Value::Bool(true));
        }
        self.checkpoint().await
    }

    /// Acts on the review: accepts the task, fails it, or sends it back for another attempt.
    async fn judge(
        &mut self,
        job: &TaskRun<'_>,
        built: Value,
        reviewed: Value,
        results: BTreeMap<String, CheckErrors>,
        (changed, after_rejection): (bool, bool),
    ) -> Result<Attempt, AgentExecutionError> {
        let task = job.task;
        let accepted = review_complete(task, &reviewed);
        if accepted {
            self.record_acceptance(job, built, &reviewed, results).await;
        }
        self.checkpoint().await?;
        if accepted {
            return Ok(Attempt::Finished(TaskOutcome::Accepted));
        }
        let status = reviewed["status"].as_str();
        if matches!(
            status,
            Some("blocked_environment" | "blocked_user" | "checkpoint")
        ) {
            let failed = explain::review_unfinished(task, &reviewed);
            return Ok(Attempt::Finished(TaskOutcome::Failed(failed)));
        }
        let reason = reviewed["summary"].as_str().unwrap_or("no summary");
        if after_rejection && !changed {
            return Ok(Attempt::Stop(Ended::Disagreement(reason.to_owned())));
        }
        Ok(Attempt::Retry(Ended::Rejected))
    }

    async fn record_acceptance(
        &mut self,
        job: &TaskRun<'_>,
        built: Value,
        reviewed: &Value,
        results: BTreeMap<String, CheckErrors>,
    ) {
        let task = job.task;
        if job.record {
            let mut done = json!({"task_id": task.id, "build": built, "review": reviewed});
            // The tree the accepted task ends with, kept under a ref.
            if let Some(checkpoint) = self.executor.accepted_checkpoint(self.request, task).await {
                done["checkpoint"] = checkpoint;
            }
            self.progress.completed.push(done);
        } else {
            self.progress.repairs.push(json!({
                "task_id": task.id,
                "instructions": task.instructions,
                "build": built,
                "review": reviewed,
            }));
        }
        let progress = &mut self.progress;
        progress.current = None;
        progress.step_plan = None;
        progress.baseline = None;
        progress.split = None;
        progress.check_errors.extend(results);
    }
}
