use harness_git_analysis::FileSignals;

use super::history_detector;
use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{file_draft, Draft},
    model::Severity,
};

/// A file bug-fixed often in the fix window, compared with the rest of the repository.
pub struct PriorDefect;

impl PriorDefect {
    fn judge(
        &self,
        s: &FileSignals,
        context: &FileContext<'_>,
        config: &DetectorConfig,
    ) -> Option<Draft<'static>> {
        let c = &config.prior_defect;
        if s.fix_commits == 0 || s.fix_percentile_per_mille < c.percentile {
            return None;
        }
        let n = s.fix_commits;
        let severity = if n >= c.critical || (n >= c.high && s.hotspot) {
            Severity::Critical
        } else if n >= c.high {
            Severity::High
        } else if n >= c.medium {
            Severity::Medium
        } else {
            Severity::Low
        };
        let days = context
            .history_summary
            .map_or(180, |summary| summary.fix_window_days);
        Some(file_draft(
            severity,
            vec![("fix_commits", n), ("percentile", s.fix_percentile_per_mille), ("window_days", days)],
            format!("Bug-fixed {n} times in the last {days} days, more than most files here: look at what keeps breaking before adding to it."),
        ))
    }
}

history_detector!(
    PriorDefect,
    "prior_defect",
    "Files bug-fixed repeatedly in recent history."
);
