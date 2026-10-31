//! Reading the plan and the checkpoint a queue run starts from.

use rusty_workflow_core::TaskQueueConfig;

use super::model::{Plan, Progress};
use super::plan::{invalid, plan, unmix_flows};
use super::task::verdict::review_complete;
use crate::orchestration::{AgentExecutionError, AgentStepRequest};

/// The plan the step's input carries, checked against the flows the queue offers.
pub(super) fn read_plan(
    request: &AgentStepRequest,
    settings: &TaskQueueConfig,
) -> Result<Plan, AgentExecutionError> {
    let source = request
        .input
        .pointer(&settings.plan_pointer)
        .ok_or_else(|| invalid("task plan is missing"))?;
    let offered = |name: &str| settings.flows.contains_key(name);
    let plan = plan(&unmix_flows(source, &offered))?;
    check_flows(&plan, settings)?;
    Ok(plan)
}

fn check_flows(plan: &Plan, settings: &TaskQueueConfig) -> Result<(), AgentExecutionError> {
    for task in &plan.tasks {
        let Some(name) = task
            .flow
            .as_deref()
            .filter(|name| !settings.flows.contains_key(*name))
        else {
            continue;
        };
        let offered: Vec<&str> = settings.flows.keys().map(String::as_str).collect();
        let offers = if offered.is_empty() {
            "none".into()
        } else {
            offered.join(", ")
        };
        return Err(invalid(format!(
            "task {} names the flow {name:?}, which this step does not offer (it offers: {offers})",
            task.id
        )));
    }
    Ok(())
}

/// The progress to continue from: the step's checkpoint, or a fresh one for a first run.
pub(super) fn read_progress(
    request: &AgentStepRequest,
    plan: &Plan,
) -> Result<Progress, AgentExecutionError> {
    let progress = match request.checkpoint.clone() {
        Some(saved) => serde_json::from_value(saved)
            .map_err(|error| invalid(format!("invalid checkpoint: {error}")))?,
        None => Progress {
            plan: Some(plan.clone()),
            ..Default::default()
        },
    };
    check_matches_plan(&progress, plan)?;
    Ok(progress)
}

/// A checkpoint is only good for the plan it was made for, with each finished task in order.
fn check_matches_plan(progress: &Progress, plan: &Plan) -> Result<(), AgentExecutionError> {
    if progress.plan.as_ref() != Some(plan) || progress.completed.len() > plan.tasks.len() {
        return Err(invalid("checkpoint does not match the task plan"));
    }
    for (task, done) in plan.tasks.iter().zip(&progress.completed) {
        let settled = done["skipped"] == true
            || done["flow"].is_string()
            || review_complete(task, &done["review"]);
        if done["task_id"] != task.id || !settled {
            return Err(invalid(
                "checkpoint contains unverified or out-of-order tasks",
            ));
        }
    }
    Ok(())
}
