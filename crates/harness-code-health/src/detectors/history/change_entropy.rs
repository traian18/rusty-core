use harness_git_analysis::FileSignals;

use super::history_detector;
use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{file_draft, percent, Draft},
    model::Severity,
};

/// A file whose changes are scattered across many commits that also touch other files
/// (Hassan's history complexity), ranked against the rest of the repository.
pub struct ChangeEntropy;

impl ChangeEntropy {
    fn judge(
        &self,
        s: &FileSignals,
        _context: &FileContext<'_>,
        config: &DetectorConfig,
    ) -> Option<Draft<'static>> {
        let c = &config.change_entropy;
        let pct = s.change_entropy_percentile_per_mille;
        if s.change_entropy_milli == 0
            || pct < c.percentile
            || s.commits_recent < c.min_recent_commits
        {
            return None;
        }
        let severity = if pct >= c.critical_percentile && s.hotspot {
            Severity::Critical
        } else if pct >= c.high_percentile {
            Severity::High
        } else {
            Severity::Medium
        };
        Some(file_draft(
            severity,
            vec![("entropy_milli", s.change_entropy_milli), ("percentile", pct), ("commits_recent", s.commits_recent)],
            format!(
                "Changes to this file are more scattered than {} of the repository's files: it is usually edited as part of wider changes, so a change here rarely stays here.",
                percent(pct)
            ),
        ))
    }
}

history_detector!(
    ChangeEntropy,
    "change_entropy",
    "Files changed mostly within scattered, many-file commits."
);
