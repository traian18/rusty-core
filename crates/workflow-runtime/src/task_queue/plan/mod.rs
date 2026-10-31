//! Reading the plan a queue runs: repairs the planner's slips, then checks what is left.
//!
//! A plan that survives is numbered, fully cross-referenced and covers every criterion; one that
//! does not is a real planning gap and is reported as `invalid_task_plan`.

mod canonical;
mod flows;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::model::{Plan, Task};
use crate::orchestration::AgentExecutionError;

pub(in crate::task_queue) use flows::unmix_flows;

const MAX_TASKS: usize = 128;

pub(in crate::task_queue) fn invalid(message: impl Into<String>) -> AgentExecutionError {
    AgentExecutionError::new("invalid_task_plan", message)
}

/// The plan in `value`, repaired and checked.
pub(in crate::task_queue) fn plan(value: &Value) -> Result<Plan, AgentExecutionError> {
    let plan: Plan = serde_json::from_value(canonical::canonicalize(value))
        .map_err(|error| invalid(error.to_string()))?;
    check_bounds(&plan)?;
    let owners = check_requirements(&plan)?;
    check_tasks(&plan, &owners)?;
    Ok(plan)
}

fn check_bounds(plan: &Plan) -> Result<(), AgentExecutionError> {
    let in_bounds = plan.status == "ready"
        && !plan.requirements.is_empty()
        && !plan.tasks.is_empty()
        && plan.tasks.len() <= MAX_TASKS;
    if in_bounds {
        return Ok(());
    }
    Err(invalid("plan needs requirements and 1–128 bounded tasks"))
}

/// Maps each criterion to its requirement while checking requirements and criteria are complete.
fn check_requirements(plan: &Plan) -> Result<HashMap<&str, &str>, AgentExecutionError> {
    let mut requirements = HashSet::new();
    let mut owners = HashMap::new();
    for requirement in &plan.requirements {
        let complete = !requirement.id.trim().is_empty()
            && !requirement.text.trim().is_empty()
            && requirements.insert(requirement.id.as_str())
            && !requirement.criteria.is_empty();
        if !complete {
            return Err(invalid(
                "requirements must have unique IDs, text and criteria",
            ));
        }
        for criterion in &requirement.criteria {
            let usable = !criterion.id.trim().is_empty()
                && !criterion.text.trim().is_empty()
                && owners
                    .insert(criterion.id.as_str(), requirement.id.as_str())
                    .is_none();
            if !usable {
                return Err(invalid("criteria need unique IDs and observable text"));
            }
        }
    }
    Ok(owners)
}

fn check_tasks(plan: &Plan, owners: &HashMap<&str, &str>) -> Result<(), AgentExecutionError> {
    let requirements: HashSet<&str> = plan.requirements.iter().map(|r| r.id.as_str()).collect();
    let mut seen = HashSet::new();
    let mut covered = HashSet::new();
    for task in &plan.tasks {
        check_task(task, &seen, &requirements, owners)?;
        covered.extend(task.criterion_ids.iter().map(String::as_str));
        seen.insert(task.id.as_str());
    }
    if covered.len() != owners.len() {
        return Err(invalid("plan leaves acceptance criteria without a task"));
    }
    Ok(())
}

fn check_task(
    task: &Task,
    earlier: &HashSet<&str>,
    requirements: &HashSet<&str>,
    owners: &HashMap<&str, &str>,
) -> Result<(), AgentExecutionError> {
    if task.instructions.trim().is_empty() {
        return Err(invalid(format!(
            "task {} has empty instructions: write them as the task's body, not on the flow line",
            task.id
        )));
    }
    let unnamed = task.id.trim().is_empty()
        || earlier.contains(task.id.as_str())
        || task.criterion_ids.is_empty()
        || task.requirement_ids.is_empty();
    if unnamed {
        return Err(invalid("tasks need unique IDs, instructions and coverage"));
    }
    if task
        .depends_on
        .iter()
        .any(|d| !earlier.contains(d.as_str()))
    {
        return Err(invalid(format!(
            "{} depends on a missing, later or cyclic task",
            task.id
        )));
    }
    if task
        .requirement_ids
        .iter()
        .any(|r| !requirements.contains(r.as_str()))
    {
        return Err(invalid("task references unknown requirement"));
    }
    for criterion in &task.criterion_ids {
        let owner = owners
            .get(criterion.as_str())
            .ok_or_else(|| invalid("task references unknown criterion"))?;
        if !task.requirement_ids.iter().any(|r| r == owner) {
            return Err(invalid("criterion is not owned by a task requirement"));
        }
    }
    Ok(())
}
