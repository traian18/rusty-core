//! What each session of a task queue is given. Every session's input is sent
//! again with each of its model calls, so it carries only what that session
//! needs:
//! - the planner (and the split) gets the whole picture to plan one task,
//!   without the text of the other tasks and with finished tasks summed up;
//! - the builder and the reviewer of a task that has a step plan get the task,
//!   its own criteria, the plan and the last attempt; the spec, the design
//!   and the earlier context reached them through the planner.
//!
//! A queue without a task planner keeps giving the builder the full input,
//! since nothing else digests it.

use serde_json::{json, Map, Value};

use super::{id_key, Plan, Progress, Task};
use crate::orchestration::AgentStepRequest;

/// Bound inputs up to this size reach the builder as they are (plan notes,
/// attachments, short preparation outputs); larger ones are the planner's.
const SMALL_FIELD_CHARS: usize = 1_500;

/// How much of a finished task's summary the next sessions see.
const DONE_SUMMARY_CHARS: usize = 400;

/// The tools a builder is not offered: what it needs comes from the step plan
/// and the code, and every offered tool's schema is sent with each call.
pub(super) const BUILDER_SKIPS: &[&str] = &[
    "repo_wiki",
    "web_search",
    "web_extract",
    "web_fetch",
    "report_progress",
];

/// The planner's input: everything bound to the step, except the text of the
/// queue's other tasks, with finished tasks as one entry each.
pub(super) fn planner_input(
    request: &AgentStepRequest,
    plan: &Plan,
    task: &Task,
    progress: &Progress,
) -> Value {
    let mut input = json!({
        "request": request.input.get("request"),
        "context": request.input.get("context"),
        "requirements": plan.requirements,
        "task": task,
        "completed_tasks": finished(progress),
        "previous_attempt": progress.current,
    });
    let skip = queue_binding(request);
    if let Some(fields) = request.input.as_object() {
        for (name, value) in fields {
            if Some(name.as_str()) != skip.as_deref() {
                input
                    .as_object_mut()
                    .unwrap()
                    .entry(name.clone())
                    .or_insert_with(|| value.clone());
            }
        }
    }
    input
}

/// The builder's (and its reviewer's) input. With a step plan: the task, its
/// own criteria, the finished tasks in a line each, the last attempt and the
/// small bound fields. Without one: the planner's input.
pub(super) fn builder_input(
    request: &AgentStepRequest,
    plan: &Plan,
    task: &Task,
    progress: &Progress,
    has_step_plan: bool,
) -> Value {
    if !has_step_plan {
        return planner_input(request, plan, task, progress);
    }
    let mut input = Map::new();
    input.insert(
        "request".into(),
        request.input.get("request").cloned().unwrap_or(Value::Null),
    );
    input.insert("task".into(), json!(task));
    input.insert(
        "requirements".into(),
        Value::Array(own_requirements(plan, task)),
    );
    input.insert("completed_tasks".into(), finished(progress));
    input.insert(
        "previous_attempt".into(),
        progress.current.clone().unwrap_or(Value::Null),
    );
    input.insert(
        "context_note".into(),
        Value::String("The spec, the design and the earlier context went to the step planner; workflow_input.step_plan is what this task needs from them. When it leaves something out, read the code, or ask spec_context or request_research when they are offered.".into()),
    );
    let skip = queue_binding(request);
    if let Some(fields) = request.input.as_object() {
        for (name, value) in fields {
            let small = value.to_string().len() <= SMALL_FIELD_CHARS;
            if small && name != "context" && Some(name.as_str()) != skip.as_deref() {
                input.entry(name.clone()).or_insert_with(|| value.clone());
            }
        }
    }
    Value::Object(input)
}

/// The input binding the queue reads its tasks from (the other tasks' text).
fn queue_binding(request: &AgentStepRequest) -> Option<String> {
    request
        .task_queue
        .as_ref()
        .and_then(|queue| queue.plan_binding())
        .map(str::to_owned)
}

/// The task's requirements with only the criteria the task covers.
fn own_requirements(plan: &Plan, task: &Task) -> Vec<Value> {
    let criteria: Vec<String> = task.criterion_ids.iter().map(|id| id_key(id)).collect();
    let requirements: Vec<String> = task.requirement_ids.iter().map(|id| id_key(id)).collect();
    plan.requirements
        .iter()
        .filter_map(|requirement| {
            let own: Vec<_> = requirement
                .criteria
                .iter()
                .filter(|criterion| criteria.contains(&id_key(&criterion.id)))
                .collect();
            (!own.is_empty() || requirements.contains(&id_key(&requirement.id)))
                .then(|| json!({"id": requirement.id, "text": requirement.text, "criteria": own}))
        })
        .collect()
}

/// The finished tasks, one entry each: what was built, in short.
fn finished(progress: &Progress) -> Value {
    Value::Array(
        progress
            .completed
            .iter()
            .map(|done| {
                let summary = done["build"]["summary"]
                    .as_str()
                    .or_else(|| done["reason"].as_str())
                    .unwrap_or("");
                let mut entry = json!({"task_id": done["task_id"], "summary": shorten(summary)});
                if done["skipped"] == true {
                    entry["skipped"] = Value::Bool(true);
                }
                entry
            })
            .collect(),
    )
}

fn shorten(text: &str) -> String {
    if text.chars().count() <= DONE_SUMMARY_CHARS {
        return text.to_owned();
    }
    let cut: String = text.chars().take(DONE_SUMMARY_CHARS).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests;
