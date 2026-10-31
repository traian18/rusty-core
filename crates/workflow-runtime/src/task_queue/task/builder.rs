//! The request a builder session starts from.

use rusty_workflow_core::StructuredOutputMode;
use serde_json::Value;

use super::verdict::result_schema;
use super::TaskRun;
use crate::orchestration::{is_user_change, AgentStepRequest};
use crate::task_queue::brief;
use crate::task_queue::queue::Queue;

impl Queue<'_> {
    pub(super) fn builder_request(&self, job: &TaskRun<'_>) -> AgentStepRequest {
        let mut child = self.request.clone();
        child.task_queue = None;
        child.checkpoint = None;
        // The user's change requests are the repair task's instructions, not something every
        // task and reviewer should act on.
        child
            .feedback
            .retain(|feedback| !is_user_change(&feedback.code));
        child.structured_output = StructuredOutputMode::HostValidated;
        child.output_schema = result_schema(false);
        child.input = brief::builder_input(
            self.request,
            self.plan,
            job.task,
            &self.progress,
            job.step_plan.is_some(),
        );
        child
            .tools
            .retain(|tool| !brief::BUILDER_SKIPS.contains(&tool.as_str()));
        if let Some(step_plan) = job.step_plan {
            child.input["step_plan"] = Value::String(step_plan.to_owned());
        }
        child.instructions = instructions(&self.request.instructions, job, &child.input);
        child
    }
}

/// The general instructions, then what this attempt adds: the plan to follow, the user's
/// guidance, and what the last attempt left behind.
fn instructions(general: &str, job: &TaskRun<'_>, input: &Value) -> String {
    let follow = if job.step_plan.is_some() {
        " Follow workflow_input.step_plan, the detailed plan for this task; where the code proves it wrong, do the smallest thing that meets the criteria and say what you did differently."
    } else {
        ""
    };
    let mut text = format!("{general}\nImplement only workflow_input.task.{follow} Preserve completed tasks. The workflow verifies the whole project after the last task, so check this task with the typecheck and the tests of what it changes, not the full suite. Return status complete only when every assigned criterion is implemented. Commands really run on this machine: when a check fails or a tool is missing, read the output and try the sensible alternatives before giving up. Never leave the implementation undone because a command fails: make every change the task needs first. If the command still cannot run, quote it and its output, end the summary with a short \"Please test\" request giving the exact command for the user to run and what to report back, and use complete. Use blocked_user only when you cannot do the task without something the user must provide. For unfinished work use checkpoint, with precise remaining work in summary. Never invent evidence.");
    // The general prompt may still ask for progress updates.
    text.push_str("\n\nreport_progress is not offered in this step: skip the progress updates the general instructions ask for, and never try to run it as a command.");
    if let Some(guidance) = job.guidance {
        // The user's own words, given when they chose to try again.
        text.push_str(&format!(
            "\n\nThe user asked you to try this task again with this guidance:\n{guidance}"
        ));
    }
    if input["previous_attempt"].get("new_errors").is_some() {
        text.push_str("\n\nYour last build added the typecheck errors in workflow_input.previous_attempt.new_errors (the workflow checked it). Fix them first; errors that were there before this task are not yours.");
    }
    if input["previous_attempt"]["unchanged"] == true {
        text.push_str("\n\nYour last attempt ended without changing any file (the workflow compared the tree), so the task was never tried. Its size, its blast radius and how widely the code is used are not blockers: they mean more mechanical edits and more tests. Do not survey or measure again what workflow_input.previous_attempt already found; make the first change of your plan now with edit_file or write_file, then continue until the step is done. Use checkpoint only if you get stuck after real edits.");
    }
    text
}
