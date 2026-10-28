//! After a pass: the measurement and the agent's report decide what the goal does next.
use serde::{Deserialize, Serialize};

use crate::{
    contract::{Goal, GoalStatus, JournalEntry, PlanStep, StepStatus, Verdict},
    measurement::{Measurement, Unmet},
    progress::{broke, compare, Progress},
};

/// What the pass itself said, besides what was measured.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassReport {
    /// The agent's last `Status:` line, lower-cased (`done`, `blocked — needs a key`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_status: Option<String>,
    /// The model judge's answer on `done_when`, asked only once every measured target is met.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_when_met: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_reason: Option<String>,
    /// The agent's last `Next:` line: what the next pass should take on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    /// The pass's to-do list as it ended; empty when it kept none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plan: Vec<PlanStep>,
}

/// Steps of `plan` that are done.
fn completed(plan: &[PlanStep]) -> usize {
    plan.iter()
        .filter(|s| s.status == StepStatus::Completed)
        .count()
}

/// What a task goal misses besides its targets: a plan, every step of it closed, and every check
/// the pass ran passing (a task has no measurement, so a failure it caused counts).
fn task_unmet(goal: &Goal, plan: &[PlanStep], now: &Measurement) -> Vec<Unmet> {
    let mut out = Vec::new();
    let open: Vec<&str> = plan
        .iter()
        .filter(|s| !s.status.is_closed())
        .map(|s| s.content.as_str())
        .collect();
    if plan.is_empty() {
        out.push(Unmet {
            target: "a plan".into(),
            detail: "no plan was kept: call update_todos with every step to the result".into(),
        });
    } else if !open.is_empty() {
        out.push(Unmet {
            target: "the plan is finished".into(),
            detail: format!("{} step(s) left: {}", open.len(), open.join("; ")),
        });
    }
    for check in now.failing_checks().filter(|c| c.ran) {
        if !goal.targets.checks.contains(&check.name) {
            out.push(Unmet {
                target: format!("{} passes", check.name),
                detail: check
                    .summary
                    .lines()
                    .take(4)
                    .collect::<Vec<_>>()
                    .join(" / "),
            });
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub verdict: Verdict,
    /// The goal's status after this pass.
    pub status: GoalStatus,
    pub unmet: Vec<Unmet>,
    /// Restore the scope to how it was before the pass.
    pub undo: bool,
    /// Passes in a row without progress, this one included.
    pub stalled: u32,
    /// Why, in one sentence for the user.
    pub reason: String,
    /// The measurement against the one before, in one line.
    pub summary: String,
}

fn blocked(report: &PassReport) -> Option<String> {
    let status = report.agent_status.as_deref()?.trim();
    let rest = status
        .get(..7)
        .filter(|head| head.eq_ignore_ascii_case("blocked"))
        .map(|_| &status[7..])?;
    Some(rest.trim_matches(|c: char| !c.is_alphanumeric()).to_owned())
}

/// The next state of `goal` after a pass measured `now`. Done only on the measurement (and the
/// judge, for `done_when`), never on the agent's word.
/// A task moves by its plan: more steps done, or fewer failing checks. New code may score lower
/// than the old, so only a broken check or more guideline errors make it worse.
fn task_progress(
    before: &Measurement,
    now: &Measurement,
    plan: &[PlanStep],
    kept: &[PlanStep],
) -> Progress {
    if broke(before, now) {
        Progress::Worse
    } else if completed(plan) > completed(kept)
        || now.failing_checks().count() < before.failing_checks().count()
    {
        Progress::Better
    } else {
        Progress::Same
    }
}

/// Every target `goal` misses, measured as `now`, with `plan` as the plan for a task goal.
pub(crate) fn unmet(goal: &Goal, plan: &[PlanStep], now: &Measurement) -> Vec<Unmet> {
    let mut out = now.unmet(&goal.targets);
    if goal.is_task() {
        out.extend(task_unmet(goal, plan, now));
    }
    out
}

pub fn decide(goal: &Goal, now: &Measurement, report: &PassReport) -> Decision {
    let plan = if report.plan.is_empty() {
        &goal.plan
    } else {
        &report.plan
    };
    let unmet = unmet(goal, plan, now);
    let before = goal.last.as_ref().or(goal.baseline.as_ref());
    let summary = match before {
        Some(before) => format!("{} (before: {})", now.headline(), before.headline()),
        None => now.headline(),
    };
    let progress = match before {
        None => Progress::Better,
        Some(before) if goal.is_task() => task_progress(before, now, plan, &goal.plan),
        Some(before) => compare(before, now, &goal.targets),
    };
    let stalled = if progress == Progress::Better {
        0
    } else {
        goal.stalled + 1
    };
    let decision = |verdict, status, undo, reason: String| Decision {
        verdict,
        status,
        unmet: unmet.clone(),
        undo,
        stalled,
        reason,
        summary: summary.clone(),
    };
    if unmet.is_empty() {
        match (goal.done_when(), report.done_when_met) {
            (None, _) | (Some(_), Some(true)) => {
                let reason = if goal.is_task() {
                    "The plan is finished, the checks pass and the result was judged done."
                } else {
                    "Every target is met."
                };
                return decision(Verdict::Done, GoalStatus::Done, false, reason.into());
            }
            (Some(_), _) => {}
        }
    }
    if let Some(what) = blocked(report) {
        let reason = if what.is_empty() {
            "The agent reported a blocker.".to_owned()
        } else {
            format!("Blocked: {what}")
        };
        return decision(Verdict::Blocked, GoalStatus::Blocked, false, reason);
    }
    let undo = progress == Progress::Worse && goal.keep_best;
    let passes = goal.passes + 1;
    if stalled >= goal.budget.no_progress_passes.max(1) {
        let reason = format!("No measurable progress in {stalled} pass(es) in a row; paused.");
        return decision(Verdict::NoProgress, GoalStatus::Paused, undo, reason);
    }
    if goal
        .budget
        .max_passes
        .is_some_and(|max| passes >= max.max(1))
    {
        let reason = format!("{passes} pass(es) used, the whole budget; paused.");
        let verdict = if undo {
            Verdict::Undone
        } else {
            Verdict::Budget
        };
        return decision(verdict, GoalStatus::Paused, undo, reason);
    }
    if undo {
        return decision(
            Verdict::Undone,
            GoalStatus::Running,
            true,
            "The pass made the measurement worse, so it was undone.".into(),
        );
    }
    let reason = match (goal.done_when(), unmet.is_empty(), &report.judge_reason) {
        (Some(_), true, Some(why)) => format!("Measured targets met; not done yet: {why}"),
        _ => format!("{} target(s) left.", unmet.len().max(1)),
    };
    decision(Verdict::Continue, GoalStatus::Running, false, reason)
}

/// Applies `decision` to `goal`: counters, the accepted measurement, status and journal.
pub fn record(
    goal: &mut Goal,
    decision: &Decision,
    now: Measurement,
    report: &PassReport,
    at: String,
) {
    goal.passes += 1;
    goal.stalled = decision.stalled;
    if !decision.undo {
        goal.last = Some(now);
        if !report.plan.is_empty() {
            goal.plan = report.plan.clone();
        }
    }
    goal.status = decision.status;
    goal.reason = (decision.status != GoalStatus::Running).then(|| decision.reason.clone());
    goal.updated_at = at.clone();
    goal.push_journal(JournalEntry {
        pass: goal.passes,
        at,
        verdict: decision.verdict,
        summary: decision.summary.clone(),
        agent_status: report.agent_status.clone(),
        next: report.next.clone(),
    });
}
