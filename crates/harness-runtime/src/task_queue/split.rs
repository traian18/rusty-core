//! Splitting a task that got stuck into smaller steps. The queue's task
//! planner gets the stuck attempt's story and writes 2-5 steps that each
//! leave the code compiling and wired in; the builder then does them one at
//! a time, and the reviewer judges the whole task as before.

use std::collections::HashMap;

use harness_core::orchestration::StructuredOutputMode;
use harness_protocol::{ids::AgentId, usage::AgentUsageMetrics};
use serde_json::{json, Value};

use super::{brief::planner_input, is_read_only, Plan, Progress, Task};
use crate::orchestration::{is_user_change, AgentExecutionError, AgentStepRequest, StepContext};
use crate::session_agent_executor::IsolatedSessionAgentExecutor;

const MIN_STEPS: usize = 2;
const MAX_STEPS: usize = 5;

const SPLIT_INSTRUCTIONS: &str = "An attempt at this task got stuck and was stopped; its changes were undone, so the code is back where the task started. workflow_input.previous_attempt says what happened (why it was stopped, the builder's account, the files it touched). Your job is to split the task into smaller steps a builder can each finish in one short attempt.

Rules for the steps:
1. 2 to 5 steps, in the order they must be done.
2. Every step leaves the project compiling and its existing tests passing. Never a step that adds a structure without wiring its first caller to it: change the structure and migrate its callers in the same step, or keep the old path working until a later step removes it.
3. The first step is the smallest change that makes the rest possible (usually: change the shared type or hook and update every caller so it compiles, with no behaviour change).
4. Name the files of each step and how to check it (the exact command).
5. Together the steps meet every criterion of workflow_input.task; say which step covers which criterion.
6. Use what the stuck attempt learned: if it found the real difficulty, put it in a step of its own.

Read the code you need with your read tools; change nothing. Reply with one section per step, exactly in this shape:
### S1 — <short title>
<what to change, in which files, and the check that proves the step is done>
### S2 — <short title>
…";

/// The steps of a split, from the planner's reply: every `### S<n>` (or
/// `## S<n>`, `### Step <n>`) section with its heading. Empty when the
/// reply has fewer than two or more than five.
pub(super) fn parse_steps(reply: &str) -> Vec<String> {
    let mut steps: Vec<String> = Vec::new();
    for line in reply.lines() {
        if is_step_heading(line) {
            steps.push(line.trim_start_matches('#').trim().to_owned());
        } else if let Some(step) = steps.last_mut() {
            step.push('\n');
            step.push_str(line);
        }
    }
    let steps: Vec<String> = steps
        .into_iter()
        .map(|step| step.trim().to_owned())
        .collect();
    if (MIN_STEPS..=MAX_STEPS).contains(&steps.len()) {
        steps
    } else {
        Vec::new()
    }
}

fn is_step_heading(line: &str) -> bool {
    let Some(title) = line
        .strip_prefix("### ")
        .or_else(|| line.strip_prefix("## "))
    else {
        return false;
    };
    let title = title.trim_start();
    let rest = title
        .strip_prefix("Step ")
        .or_else(|| title.strip_prefix('S'))
        .unwrap_or("");
    rest.starts_with(|c: char| c.is_ascii_digit())
}

/// The builder's instructions for step `index` of `steps`.
pub(super) fn step_instructions(steps: &[String], index: usize) -> String {
    let all = steps
        .iter()
        .enumerate()
        .map(|(at, step)| format!("{}. {}", at + 1, step.lines().next().unwrap_or("")))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "\n\nThis task was split into {} smaller steps after an earlier attempt got stuck (that attempt was undone). Do only step {} now; the steps before it are done. When you finish, the project must compile and its existing tests must pass. Return complete when this step is done; the review of the whole task comes after the last step.\n\nAll steps:\n{all}\n\nYour step:\n{}",
        steps.len(),
        index + 1,
        steps[index]
    )
}

impl IsolatedSessionAgentExecutor {
    /// Asks the queue's task planner to split `task`. `None` when the queue
    /// has no planner or the reply had no usable steps.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn split_task(
        &self,
        request: &AgentStepRequest,
        plan: &Plan,
        task: &Task,
        progress: &mut Progress,
        context: &mut StepContext,
        usage: &mut HashMap<AgentId, AgentUsageMetrics>,
    ) -> Result<Option<Vec<String>>, AgentExecutionError> {
        let queue = request.task_queue.as_ref().expect("task queue");
        let Some(planner) = &queue.task_planner else {
            return Ok(None);
        };
        let mut child = request.clone();
        child.task_queue = None;
        child.checkpoint = None;
        child.feedback.retain(|f| !is_user_change(&f.code));
        child.profile = Some(planner.profile.clone());
        child.instructions = format!("{}\n\n{SPLIT_INSTRUCTIONS}", planner.instructions);
        child.structured_output = StructuredOutputMode::Text;
        child.output_schema = json!({"type":"string"});
        child.input = planner_input(request, plan, task, progress);
        child.tools.retain(|t| is_read_only(t));
        let reply = match self
            .run_single_agent_step(child, context, usage)
            .await?
            .value
        {
            Value::String(text) => text,
            other => crate::markdown_reply::render_input(&other),
        };
        let steps = parse_steps(&reply);
        if steps.is_empty() {
            return Ok(None);
        }
        progress.split = Some(json!({"task_id": task.id, "steps": steps, "done": 0}));
        progress.step_plan = None;
        context
            .checkpoint(serde_json::to_value(&*progress).unwrap())
            .await?;
        Ok(Some(steps))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_read_from_their_headings() {
        let reply = "Some preamble.\n### S1 — Wire the registry\nChange `useAgentTab.ts`.\nCheck: npm run typecheck\n### S2 — Send from the owning chat\nEdit sendMessage.ts.\n## S3 Tests\nAdd tests.";
        let steps = parse_steps(reply);
        assert_eq!(steps.len(), 3);
        assert_eq!(
            steps[0],
            "S1 — Wire the registry\nChange `useAgentTab.ts`.\nCheck: npm run typecheck"
        );
        assert!(steps[2].starts_with("S3 Tests"));
    }

    #[test]
    fn too_few_or_too_many_steps_are_no_split() {
        assert!(parse_steps("### S1 — all of it\ndo it").is_empty());
        let many: String = (1..=6).map(|n| format!("### S{n} — x\ny\n")).collect();
        assert!(parse_steps(&many).is_empty());
        assert!(parse_steps("### Summary\nno steps\n### Notes\nnone").is_empty());
    }

    #[test]
    fn step_headings_may_say_step() {
        assert_eq!(parse_steps("### Step 1: a\nx\n### Step 2: b\ny").len(), 2);
    }

    #[test]
    fn a_step_names_its_place_and_the_others() {
        let steps = vec![
            "S1 — first\nbody".to_owned(),
            "S2 — second\nbody 2".to_owned(),
        ];
        let text = step_instructions(&steps, 1);
        assert!(text.contains("Do only step 2 now"));
        assert!(text.contains("1. S1 — first\n2. S2 — second"));
        assert!(text.ends_with("S2 — second\nbody 2"));
    }
}
