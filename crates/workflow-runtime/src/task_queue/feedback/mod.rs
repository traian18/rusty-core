//! What the steps after the queue and the user tell it when it is run again.
//!
//! A downstream verification may reject the result, and the user may ask for changes after
//! trying it. Neither throws the accepted work away: a rejection reopens the first task that owns
//! an unmet criterion (or becomes one repair task), and the user's changes become one repair task.

#[cfg(test)]
mod tests;

use std::collections::HashSet;

use serde_json::{json, Value};

use super::ids::id_key;
use super::model::{Plan, Progress};
use crate::orchestration::{is_user_change, AgentStepRequest};

/// Folds the feedback this run received into `progress`.
pub(super) fn absorb(request: &AgentStepRequest, plan: &Plan, progress: &mut Progress) {
    reopen_after_rejection(request, plan, progress);
    queue_user_changes(request, progress);
}

/// Re-inspects the queue after a downstream acceptance rejection instead of blindly declaring it
/// complete: reopens from the first task owning an unmet criterion, or, when none can be matched,
/// keeps the accepted tasks and schedules one repair task.
fn reopen_after_rejection(request: &AgentStepRequest, plan: &Plan, progress: &mut Progress) {
    let rejections: Vec<_> = request
        .feedback
        .iter()
        .filter(|feedback| feedback.code == "verification_failed")
        .collect();
    let Some(rejection) = rejections
        .last()
        .filter(|_| rejections.len() > progress.handled_rejections)
    else {
        return;
    };
    progress.handled_rejections = rejections.len();
    let failed = unmet_criteria(&rejection.message);
    let reopen = plan.tasks.iter().position(|task| {
        task.criterion_ids
            .iter()
            .any(|id| failed.contains(&id_key(id)))
    });
    match reopen {
        Some(at) => {
            progress.completed.truncate(at);
            progress.current = Some(json!({"rejection": rejection.message}));
        }
        None => {
            progress.pending_repair = Some(format!(
                "Fix what the final verification of the completed tasks found:\n{}",
                rejection.message
            ));
            progress.current = None;
        }
    }
}

/// The user tried the result and asked for changes: the accepted tasks stay, and one repair task
/// makes the changes after them.
fn queue_user_changes(request: &AgentStepRequest, progress: &mut Progress) {
    let changes: Vec<_> = request
        .feedback
        .iter()
        .filter(|feedback| is_user_change(&feedback.code))
        .collect();
    let Some(change) = changes
        .last()
        .filter(|_| changes.len() > progress.handled_changes)
    else {
        return;
    };
    progress.handled_changes = changes.len();
    let asked = format!(
        "The user tried the result and asked for these changes; make them and keep everything else as it is:\n{}",
        change.message
    );
    progress.pending_repair = Some(match progress.pending_repair.take() {
        Some(earlier) => format!("{earlier}\n\n{asked}"),
        None => asked,
    });
}

/// The criteria a downstream verification found unmet: the `- ID [status]` lines of its "Focus on
/// these unmet criteria" section (criteria left for manual checks are listed elsewhere and are
/// not reopened), or an older `<criteria>[...]</criteria>` JSON list.
pub(super) fn unmet_criteria(message: &str) -> HashSet<String> {
    if let Some(results) = legacy_results(message) {
        return results
            .into_iter()
            .filter(|result| result["status"] != "pass")
            .filter_map(|result| result["id"].as_str().map(id_key))
            .collect();
    }
    let Some((_, section)) = message.split_once("Focus on these unmet criteria:") else {
        return HashSet::new();
    };
    section
        .lines()
        .skip_while(|line| line.trim().is_empty())
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| line.trim().strip_prefix("- "))
        .filter_map(|line| line.split_once(" [").map(|(id, _)| id_key(id)))
        .collect()
}

fn legacy_results(message: &str) -> Option<Vec<Value>> {
    let (_, rest) = message.split_once("<criteria>")?;
    let (json, _) = rest.split_once("</criteria>")?;
    serde_json::from_str(json).ok()
}
