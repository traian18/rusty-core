use std::path::Path;

use harness_git_analysis::FileSignals;

use super::history_detector;
use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{file_draft, percent, Draft},
    model::Severity,
};

/// A busy file that changes together with many others (shotgun surgery). Tests and module index
/// files are left out: they change with their subject by design.
pub struct CoChangeScatter;

fn is_barrel(path: &str) -> bool {
    let stem = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    matches!(stem, "index" | "mod" | "__init__" | "lib")
}

impl CoChangeScatter {
    fn judge(
        &self,
        s: &FileSignals,
        context: &FileContext<'_>,
        config: &DetectorConfig,
    ) -> Option<Draft<'static>> {
        let c = &config.co_change_scatter;
        if context.test || is_barrel(context.path) {
            return None;
        }
        let pct = s.co_change_percentile_per_mille;
        if s.co_change_partners == 0
            || pct < c.percentile
            || s.commits_recent < c.min_recent_commits
        {
            return None;
        }
        let severity = if pct >= c.high_percentile {
            Severity::High
        } else {
            Severity::Medium
        };
        Some(file_draft(
            severity,
            vec![("partners", s.co_change_partners), ("percentile", pct), ("commits_recent", s.commits_recent)],
            format!(
                "Changes here repeatedly come with changes to {} other files, more than {} of the repository's files: one concern is spread across many places.",
                s.co_change_partners,
                percent(pct)
            ),
        ))
    }
}

history_detector!(
    CoChangeScatter,
    "co_change_scatter",
    "Busy files that change together with many others."
);
