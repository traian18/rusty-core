use harness_git_analysis::FileSignals;

use super::history_detector;
use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{file_draft, percent, Draft},
    model::Severity,
};

/// Many people changing a busy file at the same time with no one leading it.
pub struct DeveloperCongestion;

impl DeveloperCongestion {
    fn judge(
        &self,
        s: &FileSignals,
        _context: &FileContext<'_>,
        config: &DetectorConfig,
    ) -> Option<Draft<'static>> {
        let c = &config.developer_congestion;
        let people = s.recent_contributors;
        if people < c.contributors
            || s.churn_percentile_per_mille < c.percentile
            || s.commits_recent < c.min_recent_commits
            || s.owner_share_per_mille >= c.owner_share
        {
            return None;
        }
        let severity = if people >= c.high_contributors && s.commits_recent >= c.high_commits {
            Severity::High
        } else if people >= c.medium_contributors || s.commits_recent >= c.medium_commits {
            Severity::Medium
        } else {
            Severity::Low
        };
        Some(file_draft(
            severity,
            vec![("recent_contributors", people), ("commits_recent", s.commits_recent), ("owner_share", s.owner_share_per_mille)],
            format!(
                "{people} people changed this file in {} recent commits and none made more than {} of its changes: expect conflicts and inconsistent edits.",
                s.commits_recent,
                percent(s.owner_share_per_mille)
            ),
        ))
    }
}

history_detector!(
    DeveloperCongestion,
    "developer_congestion",
    "Busy files changed by many people with no lead."
);
