//! What each pass is asked to do: the goal, where the measurement stands, what the last passes
//! did, and how the pass is judged.
use std::fmt::Write;

use crate::{
    contract::{Goal, GoalStatus, Verdict},
    measurement::Measurement,
};

/// Findings listed per file in a pass prompt.
const FINDINGS_PER_FILE: usize = 6;
/// Journal lines a pass prompt repeats.
const JOURNAL_LINES: usize = 5;

fn round(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn measurement_section(out: &mut String, measurement: &Measurement) {
    for file in &measurement.files {
        let score = file
            .score
            .map_or_else(|| "not measured".to_owned(), |s| round(s).to_string());
        let _ = writeln!(out, "- `{}`: health {score}", file.path);
        for finding in file.findings.iter().take(FINDINGS_PER_FILE) {
            let _ = writeln!(
                out,
                "  - {} ({:?}, lines {}, worth {}): {}",
                finding.issue, finding.severity, finding.lines, finding.worth, finding.reason
            );
        }
    }
    for error in &measurement.guideline_errors {
        let _ = writeln!(
            out,
            "- guideline `{}` at `{}:{}`: {}",
            error.rule, error.path, error.line, error.message
        );
    }
    for check in measurement.failing_checks() {
        let _ = writeln!(
            out,
            "- `{}` fails: {}",
            check.name,
            check
                .summary
                .lines()
                .take(8)
                .collect::<Vec<_>>()
                .join(" / ")
        );
    }
}

/// The message pass `goal.passes + 1` is sent, measured as `current`.
pub fn pass_prompt(goal: &Goal, current: &Measurement) -> String {
    let pass = goal.passes + 1;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Goal pass {pass} of at most {}.",
        goal.budget.max_passes
    );
    let _ = writeln!(out, "\n## Goal\n{}", goal.instruction.trim());
    let _ = writeln!(out, "\n## Done when");
    let unmet = current.unmet(&goal.targets);
    if let Some(min) = goal.targets.min_file_health {
        let _ = writeln!(
            out,
            "- every file in scope has code health of at least {min} (repo_health measures it)"
        );
    }
    if let Some(max) = goal.targets.max_severity {
        let _ = writeln!(out, "- no code health finding is more severe than {max:?}");
    }
    for check in &goal.targets.checks {
        let _ = writeln!(out, "- `{check}` passes (run_check)");
    }
    if goal.targets.guidelines {
        let _ = writeln!(
            out,
            "- the project's code guidelines report no errors (code_guidelines)"
        );
    }
    if let Some(words) = &goal.targets.done_when {
        let _ = writeln!(out, "- {words}");
    }
    let scope = if goal.scope.is_empty() {
        "the files you change".to_owned()
    } else {
        goal.scope
            .iter()
            .map(|s| format!("`{s}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let _ = writeln!(out, "\nScope: {scope}.");
    let _ = writeln!(out, "\n## Where it stands\n{}", current.headline());
    measurement_section(&mut out, current);
    if !unmet.is_empty() {
        let _ = writeln!(out, "\nNot met yet:");
        for item in &unmet {
            let _ = writeln!(out, "- {}: {}", item.target, item.detail);
        }
    }
    let recent: Vec<_> = goal.journal.iter().rev().take(JOURNAL_LINES).collect();
    if !recent.is_empty() {
        let _ = writeln!(out, "\n## Earlier passes");
        for entry in recent.into_iter().rev() {
            let note = match entry.verdict {
                Verdict::Undone => " (made it worse, undone: try a different approach)",
                _ => "",
            };
            let _ = writeln!(out, "- pass {}: {}{note}", entry.pass, entry.summary);
        }
    }
    let _ = write!(
        out,
        "\n## This pass\nMake real progress towards the goal: fix the findings worth the most first, keep the code's behaviour, follow the project's guidelines, and run the checks after your last edit. The host measures the scope after you finish and decides whether the goal is met, so do not claim it is; a pass that makes the measurement worse is undone. End with `Status: done` when this pass's work is finished, or `Status: blocked — <what only the user can provide>`."
    );
    if let Some(words) = &goal.targets.done_when {
        let _ = write!(
            out,
            " Before the status line, judge the result in words (\"{}\") strictly, from the code as you leave it: `Done when: yes — <the evidence>` or `Done when: no — <what is left>`. It counts only once every measured target is met.",
            words.trim()
        );
    }
    out
}

/// The goal in a few lines, for `/goal status` and the chat.
pub fn status_text(goal: &Goal) -> String {
    let status = match goal.status {
        GoalStatus::Running => "running",
        GoalStatus::Paused => "paused",
        GoalStatus::Done => "done",
        GoalStatus::Blocked => "blocked",
        GoalStatus::Cleared => "cleared",
    };
    let mut out = format!(
        "Goal ({status}, {} of {} passes): {}",
        goal.passes,
        goal.budget.max_passes,
        goal.instruction.trim()
    );
    if let Some(reason) = &goal.reason {
        let _ = write!(out, "\n{reason}");
    }
    if let Some(last) = goal.last.as_ref().or(goal.baseline.as_ref()) {
        let _ = write!(out, "\nNow: {}", last.headline());
    }
    if let (Some(baseline), Some(_)) = (&goal.baseline, &goal.last) {
        let _ = write!(out, "\nStarted at: {}", baseline.headline());
    }
    out
}
