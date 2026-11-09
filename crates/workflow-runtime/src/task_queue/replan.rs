//! Planning the rest of a split again when a builder gets stuck on one of its steps.
//!
//! A step that comes back as checkpoint usually names what blocks it: a file the step's list left
//! out, a test the change breaks. Building the same step text again cannot get past that, so the
//! queue's task planner rewrites the steps from the stuck one onward, with the builder's account,
//! a bounded number of times per task. The steps before it stay done.

use std::collections::HashMap;

use rusty_protocol::{ids::AgentId, usage::AgentUsageMetrics};
use rusty_workflow_core::StructuredOutputMode;
use serde_json::{json, Value};

use super::{brief::planner_input, is_read_only, split, Plan, Progress, Task};
use crate::orchestration::{is_user_change, AgentExecutionError, AgentStepRequest, StepContext};
use crate::session_agent_executor::IsolatedSessionAgentExecutor;

/// How many times one task's split may be planned again.
const MAX_REPLANS: u64 = 2;

/// How many steps may replace the stuck one and those after it.
const MAX_NEW_STEPS: usize = 5;

const REPLAN_INSTRUCTIONS: &str = "This task was split into steps, and a builder got stuck on one of them. workflow_input.split_steps lists the steps, workflow_input.stuck_step is the number of the step that got stuck, and workflow_input.previous_attempt.build.summary is the builder's account of what blocks it. The steps before the stuck one are done and stay done. The stuck attempt's changes are still in the workspace: the next builder continues from them.

Rewrite the steps from the stuck one to the last so a builder can finish each one:
1. Deal with what the builder reported. When it names a file the step needs (a shared type or trait, an existing test the change breaks), put that file in the step, even if the step then has more than 4 files, or give that change a step of its own.
2. Every step leaves the project compiling and its existing tests passing.
3. Keep every criterion the old steps covered, and say which step covers which.
4. Name the files of each step and the exact command that checks it.

Read the code you need with your read tools; change nothing. Reply with one section per remaining step, 1 to 5 of them, numbered on from the stuck step (the first is S<stuck_step>), exactly in this shape:
### S<n> — <short title>
<what to change, in which files, and the check that proves the step is done>";

/// Whether `task` is in a split whose current step may be planned again.
pub(super) fn can_replan(progress: &Progress, task: &Task) -> bool {
    progress.pending_split(task).is_some() && replans(progress) < MAX_REPLANS
}

fn replans(progress: &Progress) -> u64 {
    progress
        .split
        .as_ref()
        .and_then(|split| split["replanned"].as_u64())
        .unwrap_or(0)
}

/// Replaces the steps from the stuck one onward with `fresh` and counts the replan.
pub(super) fn record_replan(progress: &mut Progress, fresh: Vec<String>) {
    let count = replans(progress) + 1;
    let Some(split) = progress.split.as_mut() else {
        return;
    };
    let done = split["done"].as_u64().unwrap_or(0) as usize;
    let mut steps: Vec<String> = serde_json::from_value(split["steps"].clone()).unwrap_or_default();
    steps.truncate(done);
    steps.extend(fresh);
    split["steps"] = json!(steps);
    split["replanned"] = json!(count);
}

impl IsolatedSessionAgentExecutor {
    /// Asks the queue's task planner to rewrite the stuck step of `task`'s split and the steps
    /// after it. False when there is no planner, no split in progress, or no usable steps.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn replan_split(
        &self,
        request: &AgentStepRequest,
        plan: &Plan,
        task: &Task,
        progress: &mut Progress,
        context: &mut StepContext,
        usage: &mut HashMap<AgentId, AgentUsageMetrics>,
    ) -> Result<bool, AgentExecutionError> {
        let Some(planner) = request
            .task_queue
            .as_ref()
            .and_then(|queue| queue.task_planner.as_ref())
        else {
            return Ok(false);
        };
        let Some((steps, done)) = progress.pending_split(task) else {
            return Ok(false);
        };
        let mut input = planner_input(request, plan, task, progress);
        input["split_steps"] = json!(steps);
        input["stuck_step"] = json!(done + 1);
        let mut child = request.clone();
        child.task_queue = None;
        child.checkpoint = None;
        child.feedback.retain(|f| !is_user_change(&f.code));
        child.profile = Some(planner.profile.clone());
        child.instructions = format!("{}\n\n{REPLAN_INSTRUCTIONS}", planner.instructions);
        child.structured_output = StructuredOutputMode::Text;
        child.output_schema = json!({"type":"string"});
        child.input = input;
        child.tools.retain(|t| is_read_only(t));
        let reply = match self
            .run_watched_agent_step(child, context, usage, super::planner_watchdog())
            .await?
            .value
        {
            Value::String(text) => text,
            other => crate::markdown_reply::render_input(&other),
        };
        let fresh = split::parse_steps_within(&reply, 1, MAX_NEW_STEPS);
        if fresh.is_empty() {
            return Ok(false);
        }
        record_replan(progress, fresh);
        context
            .checkpoint(serde_json::to_value(&*progress).unwrap())
            .await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> Task {
        Task {
            id: "T4".into(),
            requirement_ids: vec![],
            criterion_ids: vec![],
            depends_on: vec![],
            instructions: String::new(),
            flow: None,
        }
    }

    fn split_at(done: usize) -> Progress {
        let steps = ["S1 — a", "S2 — b", "S3 — c"];
        Progress {
            split: Some(json!({"task_id": "T4", "steps": steps, "done": done})),
            ..Default::default()
        }
    }

    #[test]
    fn a_replan_keeps_the_done_steps_and_replaces_the_rest() {
        let mut progress = split_at(1);
        record_replan(
            &mut progress,
            vec![
                "S2 — b with the test".into(),
                "S3 — c".into(),
                "S4 — d".into(),
            ],
        );
        let (steps, done) = progress.pending_split(&task()).unwrap();
        assert_eq!(done, 1);
        assert_eq!(
            steps,
            ["S1 — a", "S2 — b with the test", "S3 — c", "S4 — d"]
        );
    }

    #[test]
    fn a_split_is_replanned_a_bounded_number_of_times() {
        let mut progress = split_at(1);
        for _ in 0..MAX_REPLANS {
            assert!(can_replan(&progress, &task()));
            record_replan(&mut progress, vec!["S2 — again".into()]);
        }
        assert!(!can_replan(&progress, &task()));
    }

    #[test]
    fn there_is_nothing_to_replan_without_a_pending_split() {
        assert!(!can_replan(&Progress::default(), &task()));
        assert!(!can_replan(&split_at(3), &task()));
    }

    #[test]
    fn one_step_is_enough_for_a_replan() {
        assert_eq!(
            split::parse_steps_within("### S4 — with traits.rs\nedit", 1, MAX_NEW_STEPS).len(),
            1
        );
    }
}
