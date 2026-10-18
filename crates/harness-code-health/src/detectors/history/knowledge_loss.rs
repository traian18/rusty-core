use harness_git_analysis::FileSignals;

use super::history_detector;
use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{file_draft, percent, Draft},
    model::Severity,
};

/// A live file that depends on one person who has gone quiet or been replaced.
pub struct KnowledgeLoss;

impl KnowledgeLoss {
    fn judge(
        &self,
        s: &FileSignals,
        context: &FileContext<'_>,
        config: &DetectorConfig,
    ) -> Option<Draft<'static>> {
        let c = &config.knowledge_loss;
        // A stable file nobody touches is not at risk yet.
        if s.commits_recent == 0 && !s.hotspot {
            return None;
        }
        if s.bus_factor != 1 {
            return None;
        }
        let owner = s.owner.as_ref()?;
        let replaced = s
            .recent_owner
            .as_ref()
            .is_some_and(|recent| recent.id != owner.id);
        let quiet = s.owner_recent_share_per_mille < c.quiet_share;
        if !replaced && !quiet {
            return None;
        }
        let mut severity = if s.hotspot {
            Severity::High
        } else if replaced && quiet {
            Severity::Medium
        } else {
            Severity::Low
        };
        // On a team this small one owner per file is normal, not a silo.
        let small_team = context
            .history_summary
            .is_some_and(|summary| summary.active_contributors_recent <= c.small_team);
        if small_team && !s.hotspot {
            severity = Severity::Low;
        }
        let who = &owner.name;
        let reason = match (&s.recent_owner, replaced) {
            (Some(recent), true) => format!("{who} wrote most of this file, but recent changes come mostly from {}: what {who} knew about it may be leaving with them.", recent.name),
            _ => format!("{who} wrote most of this file but made only {} of its recent changes: what they knew about it may be leaving with them.", percent(s.owner_recent_share_per_mille)),
        };
        Some(file_draft(
            severity,
            vec![
                ("bus_factor", s.bus_factor),
                ("owner_share", s.owner_share_per_mille),
                ("owner_recent_share", s.owner_recent_share_per_mille),
            ],
            reason,
        ))
    }
}

history_detector!(
    KnowledgeLoss,
    "knowledge_loss",
    "Live files whose only real owner has gone quiet."
);
