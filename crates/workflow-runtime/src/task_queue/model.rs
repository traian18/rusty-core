//! The records the queue works with: the plan it was given and the progress it checkpoints.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::checks;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Criterion {
    pub(super) id: String,
    pub(super) text: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Requirement {
    pub(super) id: String,
    pub(super) text: String,
    pub(super) criteria: Vec<Criterion>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Task {
    pub(super) id: String,
    pub(super) requirement_ids: Vec<String>,
    pub(super) criterion_ids: Vec<String>,
    pub(super) depends_on: Vec<String>,
    pub(super) instructions: String,
    /// The queue's named flow that does this task, instead of the builder
    /// and reviewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) flow: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    pub(super) status: String,
    pub(super) requirements: Vec<Requirement>,
    pub(super) tasks: Vec<Task>,
    pub(super) summary: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct Progress {
    pub(super) plan: Option<Plan>,
    /// Accepted tasks, and tasks the user chose to skip (`skipped: true`).
    pub(super) completed: Vec<Value>,
    pub(super) current: Option<Value>,
    #[serde(default)]
    pub(super) handled_rejections: usize,
    /// What one repair task after the planned ones must fix: a final
    /// verification rejection no planned task could be matched to, or the
    /// changes the user asked for after trying the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pending_repair: Option<String>,
    /// How many of the user's change requests were already turned into a repair.
    #[serde(default)]
    pub(super) handled_changes: usize,
    /// Repair tasks that were accepted, so the steps after this one can see
    /// what was fixed after the planned tasks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) repairs: Vec<Value>,
    /// The task planner's detailed plan for the task in progress
    /// (`{task_id, plan}`), kept across its repairs and a resumed run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) step_plan: Option<Value>,
    /// The tree the task in progress started from (`{task_id, snapshot}`),
    /// so a stuck or failed attempt can be undone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) baseline: Option<Value>,
    /// The smaller steps the task in progress was split into after an
    /// attempt got stuck (`{task_id, steps, done}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) split: Option<Value>,
    /// Each repository's typecheck errors at the tree they were found for,
    /// so a task is judged only by the errors its build added.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) check_errors: BTreeMap<String, checks::CheckErrors>,
}

/// Marks a failure reason that is the step planner's design gap, not a failed build.
pub(super) const DESIGN_GAP: &str = "[design gap]";

/// Why a round of attempts at a task ended without an accepted task.
pub(super) enum Ended {
    /// The watchdog stopped a build.
    Stopped(String),
    /// The builder did not finish (checkpoint or needs_repair).
    Unfinished,
    /// The build added typecheck errors.
    NewErrors,
    /// The review sent back a finished build that added no errors.
    Rejected,
    /// The review asked again after a repair that changed nothing: the
    /// builder and the reviewer disagree, which another try cannot settle.
    Disagreement(String),
}

/// How a task ended, as the user is told.
pub(super) enum TaskOutcome {
    Accepted,
    Failed(String),
}

/// What happens after a task failed.
pub(super) enum AfterFailure {
    Retry(Option<String>),
    Skip,
    RevisePlan(String),
}

impl Progress {
    /// Sets one field of the in-progress record, creating the record when there is none.
    pub(super) fn annotate_current(&mut self, key: &str, value: Value) {
        self.current.get_or_insert_with(|| json!({}))[key] = value;
    }

    /// Whether `task` was already split once.
    pub(super) fn split_tried(&self, task: &Task) -> bool {
        self.split
            .as_ref()
            .is_some_and(|split| split["task_id"] == task.id.as_str())
    }

    /// The steps of `task`'s split and how many are done, while some remain.
    pub(super) fn pending_split(&self, task: &Task) -> Option<(Vec<String>, usize)> {
        let split = self
            .split
            .as_ref()
            .filter(|split| split["task_id"] == task.id.as_str())?;
        let steps: Vec<String> = serde_json::from_value(split["steps"].clone()).ok()?;
        let done = split["done"].as_u64().unwrap_or(0) as usize;
        (done < steps.len()).then_some((steps, done))
    }

    /// The first task `task` depends on that this run skipped.
    pub(super) fn skipped_dependency(&self, task: &Task) -> Option<String> {
        task.depends_on
            .iter()
            .find(|id| {
                self.completed
                    .iter()
                    .any(|done| done["skipped"] == true && done["task_id"] == id.as_str())
            })
            .cloned()
    }

    /// How many completed entries were skipped, not built.
    pub(super) fn skipped_count(&self) -> usize {
        self.completed
            .iter()
            .filter(|done| done["skipped"] == true)
            .count()
    }
}
