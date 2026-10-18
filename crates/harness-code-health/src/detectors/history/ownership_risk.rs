use harness_git_analysis::FileSignals;

use super::history_detector;
use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{file_draft, percent, Draft},
    model::Severity,
};

/// Many people each making a few changes, or no one owning most of them.
pub struct OwnershipRisk;

impl OwnershipRisk {
    fn judge(
        &self,
        s: &FileSignals,
        context: &FileContext<'_>,
        config: &DetectorConfig,
    ) -> Option<Draft<'static>> {
        let c = &config.ownership_risk;
        if s.commits < c.min_commits {
            return None;
        }
        let minor = s.minor_contributors;
        let dispersed = s.owner_share_per_mille < c.owner_share_floor;
        if minor < c.minor_contributors && !dispersed {
            return None;
        }
        let mut severity = if minor >= c.critical_minor && s.hotspot {
            Severity::Critical
        } else if minor >= c.high_minor || (minor >= c.minor_contributors && s.hotspot) {
            Severity::High
        } else if minor >= c.minor_contributors {
            Severity::Medium
        } else {
            Severity::Low
        };
        if context.metrics.nloc < c.small_nloc {
            severity = Severity::Low;
        }
        Some(file_draft(
            severity,
            vec![("minor_contributors", minor), ("owner_share", s.owner_share_per_mille), ("contributors", s.contributors), ("commits", s.commits)],
            format!(
                "{} people changed this file, {} of them only occasionally, and its main author made {} of the changes: no one holds the whole picture.",
                s.contributors,
                minor,
                percent(s.owner_share_per_mille)
            ),
        ))
    }
}

history_detector!(
    OwnershipRisk,
    "ownership_risk",
    "Files with dispersed ownership: many occasional contributors or no main author."
);
