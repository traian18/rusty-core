//! What the queue reports when every task is settled.

use serde_json::{json, Value};

use super::model::{Plan, Progress};
use crate::orchestration::AgentStepOutput;

pub(super) fn output(plan: &Plan, progress: &Progress) -> AgentStepOutput {
    let mut value = json!({
        "status": "implemented",
        "summary": summary(plan, progress),
        "tasks": progress.completed,
    });
    if !progress.repairs.is_empty() {
        value["repairs"] = Value::Array(progress.repairs.clone());
    }
    AgentStepOutput { value }
}

fn summary(plan: &Plan, progress: &Progress) -> String {
    let total = plan.tasks.len();
    let skipped = progress.skipped_count();
    let mut summary = if skipped == 0 {
        format!("Implemented and inspected {total} tasks.")
    } else {
        format!(
            "Implemented and inspected {} of {total} tasks; {skipped} skipped, at the user's request or because they build on a skipped task.",
            total - skipped
        )
    };
    if !progress.repairs.is_empty() {
        summary.push_str(&format!(
            " {} repair task(s) ran after them; see repairs.",
            progress.repairs.len()
        ));
    }
    summary.push_str(" Final validation remains required.");
    summary
}
