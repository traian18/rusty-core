//! What each pass is asked to do: the goal, where the measurement stands, what the last passes
//! did, and how the pass is judged.
use std::fmt::Write;

use crate::{
    contract::{Goal, GoalStatus, StepStatus, Targets, Verdict},
    decision::unmet,
    measurement::{FileMeasure, Measurement},
};

/// Findings listed per file in a pass prompt.
const FINDINGS_PER_FILE: usize = 6;
/// Journal lines a pass prompt repeats.
const JOURNAL_LINES: usize = 5;
/// Files of the work list shown with their findings; the rest are named with their score.
const WORK_LISTED: usize = 15;
/// Files a pass takes on at least, while that many are left.
const MIN_FILES_PER_PASS: usize = 3;

fn round(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn file_lines(out: &mut String, file: &FileMeasure) {
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

/// The files still missing a target, worst first, then guideline errors and failing checks. A
/// task goal has no target on the files, so none are listed.
fn measurement_section(out: &mut String, measurement: &Measurement, targets: &Targets) {
    let work = if targets.on_files() {
        measurement.work(targets)
    } else {
        Vec::new()
    };
    if targets.on_files() && !work.is_empty() {
        let met = measurement.scored().count().saturating_sub(work.len());
        let _ = writeln!(
            out,
            "\nWork list: {} file(s) miss a target ({met} already meet them), worst first:",
            work.len()
        );
    }
    for file in work.iter().take(WORK_LISTED) {
        file_lines(out, file);
    }
    if work.len() > WORK_LISTED {
        let rest: Vec<String> = work[WORK_LISTED..]
            .iter()
            .map(|f| match f.score {
                Some(s) => format!("`{}` {}", f.path, round(s)),
                None => format!("`{}`", f.path),
            })
            .collect();
        let _ = writeln!(out, "- also missing a target: {}", rest.join(", "));
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

/// How many files of the work list this pass takes on: its share of what is left over the passes
/// left, and at least [`MIN_FILES_PER_PASS`].
fn files_this_pass(goal: &Goal, left: usize) -> usize {
    let passes_left = goal.budget.max_passes.saturating_sub(goal.passes).max(1) as usize;
    left.div_ceil(passes_left).max(MIN_FILES_PER_PASS).min(left)
}

/// The plan as the last pass left it, one line a step.
fn plan_section(out: &mut String, goal: &Goal) {
    if goal.plan.is_empty() {
        let _ = writeln!(out, "\nNo plan yet: this pass makes it.");
        return;
    }
    let done = goal.plan.iter().filter(|s| s.status.is_closed()).count();
    let _ = writeln!(
        out,
        "\n## The plan so far ({done} of {} closed)",
        goal.plan.len()
    );
    for step in &goal.plan {
        let mark = match step.status {
            StepStatus::Pending => "[ ]",
            StepStatus::InProgress => "[>]",
            StepStatus::Completed => "[x]",
            StepStatus::Cancelled => "[-]",
        };
        let _ = writeln!(out, "{mark} {}", step.content);
    }
}

/// A task goal's pass: make the plan (or carry it on), work it through step by step, verify.
fn task_pass(out: &mut String, goal: &Goal) {
    let _ = writeln!(out, "\n## This pass");
    if goal.plan.is_empty() {
        let _ = writeln!(
            out,
            "1. Plan the whole goal first. Read what you need (repo_overview, repo_context, the files involved), then call update_todos with every step to the finished result, in order — each naming the files or area, the change, and how you will verify it. Keep steps small enough to finish and check one at a time. The plan is kept: the next pass starts from it."
        );
    } else {
        let _ = writeln!(
            out,
            "1. Carry on with the plan above: first call update_todos with it as it stands (keep the closed steps, add steps you have found, cancel a step only with the reason in its text), then take the next open steps."
        );
    }
    let _ = writeln!(
        out,
        "2. Work through the steps in order. Mark a step completed only when it is done and verified (a check, a test, or reading the result back), not when you have started it."
    );
    let _ = writeln!(
        out,
        "3. Run the checks that apply after your last edit and fix what they report. Follow the project's guidelines."
    );
    let _ = write!(
        out,
        "4. Do not end the pass while steps are open and you can still make progress. The goal ends only when every step is closed, the checks you ran pass and your judgement below is yes, so claim no more than the result shows.\n\nEnd with `Next: <the step the next pass should take first>`, then `Status: done` when this pass's work is finished, or `Status: blocked — <what only the user can provide>`."
    );
}

/// The pass's instructions: plan first, then work the plan through, then hand over.
fn this_pass(out: &mut String, goal: &Goal, current: &Measurement) {
    let left = if goal.targets.on_files() {
        current.work(&goal.targets).len()
    } else {
        0
    };
    let _ = writeln!(out, "\n## This pass");
    let _ = writeln!(
        out,
        "Work the goal down in this pass; do not stop after one small fix."
    );
    if left > 0 {
        let take = files_this_pass(goal, left);
        let _ = writeln!(
            out,
            "1. Plan first. Look at each file you take on (repo_health and repo_context; repo_health's score also counts change history, which no edit fixes, while the scores here are the code alone), then call update_todos with one item per file — at least {take} from the top of the work list ({left} left, {} pass(es) left including this one) — each naming the findings you will fix and how.",
            goal.budget.max_passes.saturating_sub(goal.passes).max(1)
        );
        let _ = writeln!(
            out,
            "2. Work through every item. A file is finished when its code findings are fixed well enough to reach the target, not after one edit; split long functions and files by responsibility, keep behaviour and public interfaces."
        );
    } else {
        let _ = writeln!(
            out,
            "1. Plan first: call update_todos with the steps that take the goal the furthest, each naming what you will change."
        );
        let _ = writeln!(out, "2. Work through every item.");
    }
    let _ = writeln!(
        out,
        "3. Run the checks after your last edit and fix what they report. Follow the project's guidelines."
    );
    let _ = write!(
        out,
        "4. Do not end the pass while planned items are unfinished and you can still make progress. The host measures the scope after you finish and decides whether the goal is met, so do not claim it is; a pass that makes the measurement worse is undone.\n\nEnd with `Next: <what the next pass should take on first>`, then `Status: done` when this pass's work is finished, or `Status: blocked — <what only the user can provide>`."
    );
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
    let unmet = unmet(goal, &goal.plan, current);
    if goal.is_task() {
        let _ = writeln!(
            out,
            "- the goal above is fully done, judged strictly from the result as you leave it"
        );
        let _ = writeln!(
            out,
            "- every step of the plan is closed (you keep it with update_todos; it carries over to the next pass)"
        );
        let _ = writeln!(out, "- every check you run passes");
    }
    if let Some(min) = goal.targets.min_mean_health {
        let _ = writeln!(
            out,
            "- the mean code health of the files in scope is at least {min}"
        );
    }
    if let Some(min) = goal.targets.min_file_health {
        let _ = writeln!(
            out,
            "- every file in scope has code health of at least {min} (its code alone, change history left out)"
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
    match goal.scope_note.as_deref().map(str::trim) {
        Some(note) if !note.is_empty() => {
            let _ = writeln!(
                out,
                "\nScope: {note}\nWork out from this which files, folders and behaviour it covers, and keep the plan within it. Measured on: {scope}."
            );
        }
        _ => {
            let _ = writeln!(out, "\nScope: {scope}.");
        }
    }
    let _ = writeln!(out, "\n## Where it stands");
    if goal.is_task() {
        plan_section(&mut out, goal);
        if !current.files.is_empty() {
            let _ = writeln!(out, "\nFiles changed so far: {}", current.headline());
        }
    } else {
        let _ = writeln!(out, "{}", current.headline());
    }
    measurement_section(&mut out, current, &goal.targets);
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
        if let Some(next) = goal.journal.last().and_then(|e| e.next.as_deref()) {
            let _ = writeln!(out, "\nThe last pass left this for you: {next}");
        }
    }
    if goal.is_task() {
        task_pass(&mut out, goal);
    } else {
        this_pass(&mut out, goal, current);
    }
    if let Some(words) = goal.done_when() {
        let what = if goal.targets.done_when.is_none() {
            "whether the goal is fully done".to_owned()
        } else {
            format!("the result in words (\"{}\")", words.trim())
        };
        let _ = write!(
            out,
            " Before the status line, judge {what} strictly, from the result as you leave it: `Done when: yes — <the evidence>` or `Done when: no — <what is left>`. It counts only once everything else above is met."
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
