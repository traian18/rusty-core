//! After a pass: the measurement and the agent's report decide what the goal does next.
use serde::{Deserialize, Serialize};

use crate::{
    contract::{Goal, GoalStatus, JournalEntry, Verdict},
    measurement::{Measurement, Unmet},
    progress::{compare, Progress},
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
pub fn decide(goal: &Goal, now: &Measurement, report: &PassReport) -> Decision {
    let unmet = now.unmet(&goal.targets);
    let before = goal.last.as_ref().or(goal.baseline.as_ref());
    let summary = match before {
        Some(before) => format!("{} (before: {})", now.headline(), before.headline()),
        None => now.headline(),
    };
    let progress = before.map_or(Progress::Better, |b| compare(b, now, &goal.targets));
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
        match (&goal.targets.done_when, report.done_when_met) {
            (None, _) | (Some(_), Some(true)) => {
                return decision(
                    Verdict::Done,
                    GoalStatus::Done,
                    false,
                    "Every target is met.".into(),
                )
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
    if passes >= goal.budget.max_passes.max(1) {
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
    let reason = match (
        &goal.targets.done_when,
        unmet.is_empty(),
        &report.judge_reason,
    ) {
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
    });
}
