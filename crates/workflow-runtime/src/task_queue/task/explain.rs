//! Telling the user why a task did not get through.

use serde_json::Value;

use crate::task_queue::model::{Ended, Progress, Task, DESIGN_GAP};

pub(super) fn design_gap(task: &Task, question: &str) -> String {
    format!(
        "{DESIGN_GAP} Task {} needs a decision before it can be built. The step planner asks:\n{question}\nTry again with your answer as guidance, or revise the design and the task list",
        task.id
    )
}

/// Why the build is kept rather than undone, when the review is what keeps rejecting it.
pub(super) fn kept(ended: &Ended, max_repairs: u32, progress: &Progress) -> Option<String> {
    match ended {
        Ended::Disagreement(reason) => Some(format!(
            "the review sent it back again after a repair that changed nothing, so the builder and the reviewer disagree. The review said: {reason}"
        )),
        Ended::Rejected => Some(format!(
            "the review still sends it back after {} attempts, although the build adds no typecheck errors. The last review said: {}",
            max_repairs + 1,
            last_review_summary(progress)
        )),
        _ => None,
    }
}

fn last_review_summary(progress: &Progress) -> &str {
    progress
        .current
        .as_ref()
        .and_then(|current| current["review"]["summary"].as_str())
        .unwrap_or("no summary")
}

pub(super) fn kept_work(task: &Task, why: &str) -> String {
    format!(
        "Task {} was not accepted: {why}. Its work is kept: try again with guidance for the builder, skip the task, or revise the plan",
        task.id
    )
}

/// Why the attempt is undone.
pub(super) fn undone(ended: &Ended, max_repairs: u32) -> String {
    let attempts = max_repairs + 1;
    match ended {
        Ended::Stopped(reason) => format!("the workflow stopped its build: {reason}"),
        Ended::NewErrors => {
            format!("its build still added typecheck errors after {attempts} attempts")
        }
        _ => format!("the builder did not finish it in {attempts} attempts"),
    }
}

pub(super) fn not_completed(task: &Task, why: &str) -> String {
    format!(
        "Task {} could not be completed: {why}. Its last attempt was undone; completed tasks are checkpointed",
        task.id
    )
}

/// The builder stopped with a status that ends the task.
pub(super) fn builder_stopped(task: &Task, status: &str, built: &Value) -> String {
    let why = if status == "blocked_user" {
        "it needs something from you"
    } else {
        "the builder could not finish it"
    };
    format!(
        "Task {} stopped because {why}: {}",
        task.id,
        built["summary"].as_str().unwrap_or("no summary")
    )
}

/// The review could not reach a verdict.
pub(super) fn review_unfinished(task: &Task, reviewed: &Value) -> String {
    format!(
        "The review of task {} could not finish: {}",
        task.id,
        reviewed["summary"].as_str().unwrap_or("no summary")
    )
}
