//! How the queue recovers from a build attempt that got stuck: it watches
//! every build, keeps the tree each task (and each accepted task) ends with,
//! and undoes a stopped or failed attempt so the next one starts clean.

use std::collections::HashMap;

use harness_protocol::{ids::AgentId, usage::AgentUsageMetrics};
use serde_json::{json, Value};

use super::snapshot::{self, Snapshot};
use super::{Progress, Task};
use crate::orchestration::{AgentExecutionError, AgentStepRequest, StepContext};
use crate::session_agent_executor::watchdog::{Limits, Watchdog, WATCHDOG_STOPPED};
use crate::session_agent_executor::IsolatedSessionAgentExecutor;

/// The result of one build.
pub(super) enum Built {
    /// The builder replied (any status).
    Replied(Value),
    /// The watchdog stopped the build; why, and the builder's last word.
    Stopped(String),
}

/// How much of an undone attempt's diff is kept for the planner.
const UNDONE_DIFF_CHARS: usize = 12_000;

fn label(request: &AgentStepRequest, task: &Task, what: &str) -> String {
    format!("{}/{}-{what}", request.run_id, task.id)
}

impl IsolatedSessionAgentExecutor {
    /// Runs one build under a fresh watchdog.
    pub(super) async fn build_watched(
        &self,
        child: AgentStepRequest,
        context: &mut StepContext,
        usage: &mut HashMap<AgentId, AgentUsageMetrics>,
    ) -> Result<Built, AgentExecutionError> {
        let watchdog = Watchdog::new(Limits::default());
        let result = self
            .run_watched_agent_step(child, context, usage, watchdog.clone())
            .await;
        match (result, watchdog.stopped()) {
            (Err(error), _) if error.code == "cancelled" => Err(error),
            // However the stopped run ended (cancelled by the watchdog, an
            // unpassed gate after the stop), the attempt counts as stopped.
            (Err(_), Some(reason)) => Ok(Built::Stopped(reason)),
            (Err(error), None) if error.code == WATCHDOG_STOPPED => {
                Ok(Built::Stopped(error.message))
            }
            (Err(error), None) => Err(error),
            (Ok(output), Some(reason)) => {
                Ok(Built::Stopped(match output.value["summary"].as_str() {
                    Some(summary) => format!("{reason}\n\nThe builder's account:\n{summary}"),
                    None => reason,
                }))
            }
            (Ok(output), None) => Ok(Built::Replied(output.value)),
        }
    }

    /// The tree the task started from: the one kept from an earlier attempt
    /// of this task, else the current one, recorded now.
    pub(super) async fn baseline(
        &self,
        request: &AgentStepRequest,
        task: &Task,
        progress: &mut Progress,
    ) -> Option<Snapshot> {
        let kept = progress
            .baseline
            .as_ref()
            .filter(|kept| kept["task_id"] == task.id.as_str())
            .and_then(|kept| serde_json::from_value(kept["snapshot"].clone()).ok());
        if kept.is_some() {
            return kept;
        }
        let taken = self.snapshot(label(request, task, "start")).await?;
        progress.baseline = Some(json!({"task_id": task.id, "snapshot": taken}));
        Some(taken)
    }

    /// Makes the current tree the task's baseline: after a step of a split
    /// task, so a later stop undoes only the step that got stuck.
    pub(super) async fn advance_baseline(
        &self,
        request: &AgentStepRequest,
        task: &Task,
        progress: &mut Progress,
        what: &str,
    ) {
        if let Some(taken) = self.snapshot(label(request, task, what)).await {
            progress.baseline = Some(json!({"task_id": task.id, "snapshot": taken}));
        }
    }

    /// Records the tree an accepted task ends with, for its completed entry.
    pub(super) async fn accepted_checkpoint(
        &self,
        request: &AgentStepRequest,
        task: &Task,
    ) -> Option<Value> {
        let taken = self.snapshot(label(request, task, "accepted")).await?;
        serde_json::to_value(taken).ok()
    }

    /// Undoes everything since the task's baseline. The undone attempt is
    /// kept under its own ref, and `progress.current` says what happened so
    /// the next builder knows. Quiet when there is nothing to undo.
    pub(super) async fn undo_attempt(
        &self,
        request: &AgentStepRequest,
        task: &Task,
        progress: &mut Progress,
        why: &str,
    ) {
        let Some(baseline) = progress
            .baseline
            .as_ref()
            .filter(|kept| kept["task_id"] == task.id.as_str())
            .and_then(|kept| serde_json::from_value::<Snapshot>(kept["snapshot"].clone()).ok())
        else {
            return;
        };
        let root = self.workspace_root();
        let attempt = label(request, task, &format!("undone-{}", now_millis()));
        // The undone attempt's diff goes with the note, so a planner can
        // reuse what it got right instead of exploring again.
        let undone = tokio::task::spawn_blocking(move || {
            let undone = snapshot::undo_to(&root, &baseline, &attempt)?;
            let changes = snapshot::patch(&root, &baseline, &undone.attempt, UNDONE_DIFF_CHARS);
            Ok((undone, changes))
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result);
        let note = match undone {
            Ok((undone, _)) if undone.files.is_empty() => return,
            Ok((undone, changes)) => json!({
                "why": why,
                "files_put_back": undone.files,
                "attempt_kept_at": undone.attempt.repos.iter().map(|repo| json!({"repo": repo.repo, "commit": repo.commit})).collect::<Vec<_>>(),
                "changes": changes,
            }),
            Err(error) => {
                json!({"why": why, "error": format!("the attempt could not be undone: {error}")})
            }
        };
        let current = progress
            .current
            .get_or_insert_with(|| json!({"task_id": task.id}));
        current["undone"] = note;
    }

    /// Every repository's working tree, to tell whether a build changed
    /// anything.
    pub(super) async fn tree_fingerprint(&self) -> Vec<(String, String)> {
        let root = self.workspace_root();
        tokio::task::spawn_blocking(move || snapshot::fingerprint(&root))
            .await
            .unwrap_or_default()
    }

    async fn snapshot(&self, label: String) -> Option<Snapshot> {
        let root = self.workspace_root();
        tokio::task::spawn_blocking(move || snapshot::take(&root, &label))
            .await
            .ok()
            .filter(|taken| !taken.repos.is_empty())
    }
}

/// A unique, sortable suffix for an undone attempt's ref.
fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default()
}

/// Whether the task's last attempt should be undone before trying again: it
/// did not finish (any status but complete), it was stopped, or the user
/// asked for another try. A finished build the review sent back is kept, so
/// its repair builds on it; so is one the review kept rejecting (`kept`),
/// even when the user asks for another try.
pub(super) fn needs_undo(progress: &Progress, task: &Task, guidance: Option<&str>) -> bool {
    let Some(current) = progress
        .current
        .as_ref()
        .filter(|current| current["task_id"] == task.id.as_str())
    else {
        return false;
    };
    if current.get("undone").is_some() || current["kept"] == true {
        return false;
    }
    guidance.is_some()
        || current.get("stopped").is_some()
        || current["build"]["status"] != "complete"
}
