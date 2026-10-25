//! The queue's own typecheck around each task. Errors a build added, compared
//! with the errors its repository had when the task started, send the task
//! back to the builder before any review; the reviewer is told which errors
//! were already there, so it neither waves new ones through nor blames the
//! task for old ones.
//!
//! The check is the host's `run_check` tool (typecheck, per repository). A
//! host without it, or a repository without a typecheck, is not checked.
//! Results are kept per repository with the tree they are for, so a task
//! that starts where the last accepted one ended needs no check of its own.

use std::collections::BTreeMap;

use harness_tools::ToolInput;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{snapshot, Progress, Task};
use crate::session_agent_executor::IsolatedSessionAgentExecutor;

/// How many new errors are quoted to the builder.
const QUOTED_ERRORS: usize = 20;

/// A repository's typecheck errors for one tree.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct CheckErrors {
    pub tree: String,
    pub count: u32,
    /// The errors without their line and column, so moved code still matches.
    pub errors: Vec<String>,
}

/// What the check found after a build.
#[derive(Debug, Default)]
pub(super) struct AfterBuild {
    /// Errors the build added, for the builder (empty: none).
    pub added: Vec<String>,
    /// For the reviewer: what was checked and what was there before.
    pub note: Option<String>,
    /// The results to keep when the task is accepted.
    pub results: BTreeMap<String, CheckErrors>,
}

/// The errors in a `run_check` result: none when it passed; `None` when it
/// failed without listing errors (a missing tool, no typecheck), which is
/// not a result to compare.
pub(super) fn parse(failed: bool, text: &str) -> Option<(u32, Vec<String>)> {
    if !failed {
        return Some((0, Vec::new()));
    }
    let errors: Vec<String> = text
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("- "))
        .filter(|line| has_location(line))
        .map(without_location)
        .collect();
    let count = error_count(text).unwrap_or(errors.len() as u32);
    (count > 0).then_some((count, errors))
}

/// Errors in `after` that `before` did not have (as many times as they
/// were added), or, when the list was cut, a line saying the count grew.
pub(super) fn added(before: &CheckErrors, count: u32, errors: &[String]) -> Vec<String> {
    let mut known: BTreeMap<&str, u32> = BTreeMap::new();
    for error in &before.errors {
        *known.entry(error).or_default() += 1;
    }
    let mut new = Vec::new();
    for error in errors {
        match known.get_mut(error.as_str()) {
            Some(left) if *left > 0 => *left -= 1,
            _ => new.push(error.clone()),
        }
    }
    if new.is_empty() && count > before.count {
        new.push(format!(
            "{count} errors now, {} before this task",
            before.count
        ));
    }
    new
}

/// `path:12:5 [TS2345] message` (or `path:12: message`): a located error.
fn has_location(line: &str) -> bool {
    line.split_once(':')
        .is_some_and(|(_, rest)| rest.starts_with(|c: char| c.is_ascii_digit()))
}

fn without_location(line: &str) -> String {
    let Some((path, mut rest)) = line.split_once(':') else {
        return line.to_owned();
    };
    // Drop `12`, `12:5`, and the separator after them.
    loop {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            break;
        }
        rest = &rest[digits..];
        match rest.strip_prefix(':') {
            Some(after) => rest = after,
            None => break,
        }
    }
    format!("{path} {}", rest.trim())
}

/// The first "N errors" (or "N error") in a check's output.
fn error_count(text: &str) -> Option<u32> {
    let words: Vec<&str> = text.split_whitespace().collect();
    words.windows(2).find_map(|pair| {
        let word = pair[1].trim_end_matches(|c: char| !c.is_alphanumeric());
        (word == "errors" || word == "error")
            .then(|| pair[0].parse().ok())
            .flatten()
    })
}

/// The text of a tool result (a failure comes as `{"error": "..."}`).
fn output_text(output: &Value) -> String {
    match output {
        Value::String(text) => text.clone(),
        Value::Object(fields) => fields
            .values()
            .map(output_text)
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// The repositories a task works in, as far as its text says: the ones it
/// names, or all of them when it names none (or the workspace is one).
pub(super) fn named_repos(
    repos: &[(String, String)],
    task: &Task,
    step_plan: Option<&str>,
) -> Vec<(String, String)> {
    let text = format!("{}\n{}", task.instructions, step_plan.unwrap_or(""));
    let named: Vec<_> = repos
        .iter()
        .filter(|(name, _)| !name.is_empty() && text.contains(name.as_str()))
        .cloned()
        .collect();
    if named.is_empty() {
        repos.to_vec()
    } else {
        named
    }
}

impl IsolatedSessionAgentExecutor {
    /// Runs the typecheck of `repo` ("" for the workspace root). `None` when
    /// the host has no `run_check` or the repository no typecheck.
    async fn typecheck(
        &self,
        repo: &str,
        cancel: &CancellationToken,
    ) -> Option<(u32, Vec<String>)> {
        let tool = self.host_tool("run_check")?;
        let mut arguments = json!({"check": "typecheck"});
        if !repo.is_empty() {
            arguments["path"] = Value::String(repo.to_owned());
        }
        let result = tool
            .execute(ToolInput { arguments }, cancel.clone())
            .await
            .ok()?;
        parse(result.is_error, &output_text(&result.output))
    }

    /// Records the typecheck errors of the repositories `task` works in, as
    /// they are before its build. Repositories already checked at their
    /// current tree (by the last accepted task) are not checked again.
    pub(super) async fn check_before_build(
        &self,
        task: &Task,
        step_plan: Option<&str>,
        progress: &mut Progress,
        cancel: &CancellationToken,
    ) {
        if self.host_tool("run_check").is_none() {
            return;
        }
        let root = self.workspace_root();
        let repos = tokio::task::spawn_blocking(move || snapshot::fingerprint(&root))
            .await
            .unwrap_or_default();
        for (repo, tree) in named_repos(&repos, task, step_plan) {
            if progress
                .check_errors
                .get(&repo)
                .is_some_and(|known| known.tree == tree)
            {
                continue;
            }
            match self.typecheck(&repo, cancel).await {
                Some((count, errors)) => {
                    progress.check_errors.insert(
                        repo,
                        CheckErrors {
                            tree,
                            count,
                            errors,
                        },
                    );
                }
                None => {
                    progress.check_errors.remove(&repo);
                }
            }
        }
    }

    /// Typechecks the repositories the build changed that have a result from
    /// before it, and says what it added.
    pub(super) async fn check_after_build(
        &self,
        progress: &Progress,
        cancel: &CancellationToken,
    ) -> AfterBuild {
        let mut after = AfterBuild::default();
        if progress.check_errors.is_empty() {
            return after;
        }
        let root = self.workspace_root();
        let repos = tokio::task::spawn_blocking(move || snapshot::fingerprint(&root))
            .await
            .unwrap_or_default();
        let mut notes = Vec::new();
        for (repo, tree) in repos {
            let Some(before) = progress.check_errors.get(&repo) else {
                continue;
            };
            if before.tree == tree {
                continue;
            }
            let Some((count, errors)) = self.typecheck(&repo, cancel).await else {
                continue;
            };
            let name = if repo.is_empty() {
                "the project"
            } else {
                repo.as_str()
            };
            let added = added(before, count, &errors);
            if added.is_empty() {
                notes.push(match (count, before.count) {
                    (0, _) => format!("The typecheck of {name} passes."),
                    (now, was) => format!("The typecheck of {name} has {now} errors; {was} were there before this task started, and the build added none, so they are not this task's to fix."),
                });
            } else {
                after.added.extend(
                    added
                        .iter()
                        .take(QUOTED_ERRORS)
                        .map(|error| format!("{name}: {error}")),
                );
            }
            after.results.insert(
                repo,
                CheckErrors {
                    tree,
                    count,
                    errors,
                },
            );
        }
        if !notes.is_empty() {
            after.note = Some(format!(
                "The workflow ran the typecheck after this build. {}",
                notes.join(" ")
            ));
        }
        after
    }
}

#[cfg(test)]
mod tests;
