//! Splitting a task that got stuck into smaller steps. The queue's task
//! planner gets the stuck attempt's story and writes 2-5 steps that each
//! leave the code compiling and wired in; the builder then does them one at
//! a time, and the reviewer judges the whole task as before.

use std::collections::HashMap;

use rusty_protocol::{ids::AgentId, usage::AgentUsageMetrics};
use rusty_workflow_core::StructuredOutputMode;
use serde_json::{json, Value};

use super::{brief::planner_input, is_read_only, Plan, Progress, Task};
use crate::orchestration::{is_user_change, AgentExecutionError, AgentStepRequest, StepContext};
use crate::session_agent_executor::IsolatedSessionAgentExecutor;

const MIN_STEPS: usize = 2;
const MAX_STEPS: usize = 5;

const SPLIT_INSTRUCTIONS: &str = "An attempt at this task did not succeed and its changes were undone, so the code is back where the task started. workflow_input.previous_attempt says what happened: undone.why says why it was undone, undone.changes is the diff of what the attempt changed, and the builder's account and the review, when there was one, follow. Start from that diff: keep what it got right, and read only the code you still need. Your job is to split the task into smaller steps a builder can each finish in one short attempt.

Rules for the steps:
1. 2 to 5 steps, in the order they must be done.
2. Every step leaves the project compiling and its existing tests passing. Never a step that adds a structure without wiring its first caller to it: change the structure and migrate its callers in the same step, or keep the old path working until a later step removes it.
3. The first step is the smallest change that makes the rest possible (usually: change the shared type or hook and update every caller so it compiles, with no behaviour change).
4. Name the files of each step and how to check it (the exact command). A step's files include every file its check needs to pass, such as an existing test the change breaks or a shared type it must change.
5. Together the steps meet every criterion of workflow_input.task; say which step covers which criterion.
6. Use what the undone attempt learned: if it found the real difficulty, put it in a step of its own.

Read the code you need with your read tools; change nothing. Reply with one section per step, exactly in this shape:
### S1 — <short title>
<what to change, in which files, and the check that proves the step is done>
### S2 — <short title>
…";

/// Appended to the step planner's instructions: before planning, it judges
/// whether the task fits one builder session, and may split it up front or
/// stop it on a decision only the person can make.
pub(super) const FIT_INSTRUCTIONS: &str = "Before you plan, judge whether this task fits one builder session: it changes at most 4 files (tests included), and nothing is left to discover or decide. Then reply in exactly one of these three shapes:
1. It fits: your whole reply is the step plan, as described above.
2. It is too big: reply with the heading `## Split`, then 2 to 5 steps in the order they must be done, each a section `### S1 — <short title>` naming its files (at most 4) and the exact command that checks it. A step's files include every file its check needs to pass: an existing test the change breaks (a test that asserts the exact events, output or order the change alters), a shared type or trait it must change. When that takes a step past 4 files, split it further rather than leave a needed file out. Every step leaves the project compiling and its tests passing; a step that changes a shared type or hook also updates every caller. The first step is the smallest change that makes the rest possible. Together the steps meet every criterion of workflow_input.task; say which step covers which.
3. It cannot be built without a decision only the person can make (the design relies on something that is not in the code and does not say how to create it, or the spec and the design allow two different results): reply with the heading `## Design gap`, then the question, the options with what each means for the code, and your recommended answer. Use it only when no careful reading of the design and the code settles the question: a detail you can settle from the code is yours to settle in the plan.";

/// Appended to the step planner's instructions: the plan carries what the
/// planner read, so the builder and the reviewer do not read it again.
pub(super) const MATERIAL_INSTRUCTIONS: &str = "A step plan ends with a section `## Material`: what you read that the builder and the reviewer need, quoted so they do not read it again (they start with nothing else). Quote only the design and spec passages this task follows, and the code it changes or calls, each introduced by `path:start-end` and given exactly as in the file. Keep the section under about 6,000 characters: quote the lines that matter, never whole files, and name a location without quoting it when its content does not matter. workflow_input.known_locations lists where earlier tasks found things: use it instead of searching again.";

/// What the step planner replied: a plan, steps to build instead, or a
/// question for the person.
#[derive(Debug, PartialEq)]
pub(super) enum Planned {
    Plan(String),
    Split(Vec<String>),
    Gap(String),
}

/// Reads the step planner's reply. A `## Split` whose steps cannot be read
/// is taken as a plan, so the builder still gets the planner's work.
pub(super) fn read_planned(reply: &str) -> Planned {
    let after = |heading: &str| {
        let mut at = 0;
        for line in reply.split_inclusive('\n') {
            if line.trim().eq_ignore_ascii_case(heading) {
                return Some(reply[at + line.len()..].trim());
            }
            at += line.len();
        }
        None
    };
    if let Some(question) = after("## Design gap").filter(|q| !q.is_empty()) {
        return Planned::Gap(question.to_owned());
    }
    if let Some(steps) = after("## Split").map(parse_steps) {
        if !steps.is_empty() {
            return Planned::Split(steps);
        }
    }
    Planned::Plan(reply.to_owned())
}

/// Records `steps` as the split of `task`, built from its first step.
pub(super) fn record_split(progress: &mut Progress, task: &Task, steps: &[String]) {
    progress.split = Some(json!({"task_id": task.id, "steps": steps, "done": 0}));
    progress.step_plan = None;
}

/// The steps of a split, from the planner's reply: every `### S<n>` (or
/// `## S<n>`, `### Step <n>`) section with its heading. Empty when the
/// reply has fewer than two or more than five.
pub(super) fn parse_steps(reply: &str) -> Vec<String> {
    parse_steps_within(reply, MIN_STEPS, MAX_STEPS)
}

/// The step sections of `reply`, or none when there are fewer than `min` or more than `max`.
pub(super) fn parse_steps_within(reply: &str, min: usize, max: usize) -> Vec<String> {
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
    if (min..=max).contains(&steps.len()) {
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
        "\n\nThis task was split into {} smaller steps, each small enough for one session. Do only step {} now; the steps before it are done. When you finish, the project must compile and its existing tests must pass. Return complete when this step is done; the review of the whole task comes after the last step. The files your step names are its scope. If its check cannot pass without changing a file it does not name (a shared type, an existing test your change breaks), do not change that file and do not widen the scope yourself: return checkpoint, and say in the summary which file the step needs and why. The workflow then plans the remaining steps again with it.\n\nAll steps:\n{all}\n\nYour step:\n{}",
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
        let Some(planner) = request
            .task_queue
            .as_ref()
            .and_then(|queue| queue.task_planner.as_ref())
        else {
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
            .run_watched_agent_step(child, context, usage, super::planner_watchdog())
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
        record_split(progress, task, &steps);
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
    fn the_planner_reply_is_a_plan_a_split_or_a_design_gap() {
        assert_eq!(
            read_planned("1. Edit `a.rs`.\n2. Run `cargo test`."),
            Planned::Plan("1. Edit `a.rs`.\n2. Run `cargo test`.".into())
        );
        let split = read_planned(
            "Too big for one session.\n## Split\n### S1 — Add the field\nbackend.rs\n### S2 — Assign ordinals\nagent_runner.rs",
        );
        assert_eq!(
            split,
            Planned::Split(vec![
                "S1 — Add the field\nbackend.rs".into(),
                "S2 — Assign ordinals\nagent_runner.rs".into()
            ])
        );
        assert_eq!(
            read_planned("## Design gap\nWhere does `routeId` come from? Options: …"),
            Planned::Gap("Where does `routeId` come from? Options: …".into())
        );
        // A split with one step is not a split: the planner's text is still a plan.
        assert!(matches!(
            read_planned("## Split\n### S1 — all of it"),
            Planned::Plan(_)
        ));
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
