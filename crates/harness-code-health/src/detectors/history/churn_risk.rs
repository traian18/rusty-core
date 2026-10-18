use harness_git_analysis::FileSignals;

use super::history_detector;
use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{file_draft, Draft},
    model::Severity,
};

/// A file rewritten more than its own size in the recent window, measured against its size so
/// big files are not flagged just for being big.
pub struct ChurnRisk;

impl ChurnRisk {
    fn judge(
        &self,
        s: &FileSignals,
        context: &FileContext<'_>,
        config: &DetectorConfig,
    ) -> Option<Draft<'static>> {
        let c = &config.churn_risk;
        let nloc = context.metrics.nloc;
        if nloc == 0
            || s.commits_recent < c.min_recent_commits
            || s.churn_percentile_per_mille < c.percentile
        {
            return None;
        }
        let relative = (s.lines_changed_recent.saturating_mul(1000) / u64::from(nloc))
            .min(u64::from(u32::MAX)) as u32;
        if relative < c.relative {
            return None;
        }
        let severity = if relative >= c.critical_relative && s.hotspot {
            Severity::Critical
        } else if relative >= c.high_relative {
            Severity::High
        } else if relative >= c.medium_relative {
            Severity::Medium
        } else {
            Severity::Low
        };
        Some(file_draft(
            severity,
            vec![("relative_churn_milli", relative), ("lines_changed_recent", s.lines_changed_recent.min(u64::from(u32::MAX)) as u32), ("nloc", nloc), ("commits_recent", s.commits_recent)],
            format!(
                "{} lines changed in {} recent commits, {:.1} times the file's {} lines of code: it is being rewritten faster than it settles.",
                s.lines_changed_recent,
                s.commits_recent,
                f64::from(relative) / 1000.0,
                nloc
            ),
        ))
    }
}

history_detector!(
    ChurnRisk,
    "churn_risk",
    "Files rewritten more than their own size recently."
);
