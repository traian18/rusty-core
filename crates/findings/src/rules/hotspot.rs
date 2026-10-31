use super::{FindingRule, FindingsInput, Requirements};
use crate::{
    model::{Confidence, Draft, Evidence, FindingKind, GitEvidence, Severity},
    view::Walk,
};

/// Files that change a lot and that many others depend on.
pub struct ChangeHotspotRule;

impl FindingRule for ChangeHotspotRule {
    fn id(&self) -> &'static str {
        "change-hotspot"
    }
    fn version(&self) -> u32 {
        1
    }
    fn kind(&self) -> FindingKind {
        FindingKind::Candidate
    }
    fn requires(&self) -> Requirements {
        Requirements {
            groups: false,
            git: true,
        }
    }
    fn description(&self) -> &'static str {
        "Candidate: files in the top churn percentile with many transitive dependents."
    }
    fn evaluate(&self, input: &FindingsInput<'_>) -> Vec<Draft> {
        let Some(git) = input.git else {
            return Vec::new();
        };
        let config = input.config;
        let mut commits = git
            .index
            .churn
            .iter()
            .map(|c| c.commits)
            .collect::<Vec<_>>();
        commits.sort_unstable();
        // Nearest-rank percentile: the smallest value at or above the requested share of files.
        let rank = (commits.len() * config.hotspot_churn_percentile as usize)
            .div_ceil(100)
            .max(1);
        let Some(&threshold) = commits.get(rank - 1) else {
            return Vec::new();
        };
        let threshold = threshold.max(config.hotspot_min_commits);
        git.index
            .churn
            .iter()
            .filter(|churn| churn.commits >= threshold && !churn.absent_at_head && input.view.has_file(&churn.file))
            .filter_map(|churn| {
                let dependents = input.view.reachable(&churn.file, config.hotspot_dependent_hops, Walk::ImportedBy);
                (dependents.len() >= config.hotspot_min_dependents).then_some((churn, dependents.len()))
            })
            .map(|(churn, dependents)| {
                let severe = churn.commits >= 20 && dependents >= 25;
                Draft {
                    key: churn.file.as_path().display().to_string(),
                    severity: if severe { Severity::Medium } else { Severity::Low },
                    confidence: Confidence::Medium,
                    title: format!("{} changes often and many files depend on it", churn.file.as_path().display()),
                    summary: format!(
                        "{} commits in the analysed window (top {}% of files) and {} files depend on it within {} hops.",
                        churn.commits, 100 - config.hotspot_churn_percentile, dependents, config.hotspot_dependent_hops
                    ),
                    rationale: "Frequent change in a widely used file spreads risk: each edit can affect many dependents. Consider whether it mixes several responsibilities, or whether a smaller stable interface could sit in front of the volatile part.".to_owned(),
                    subjects: vec![churn.file.clone()],
                    packages: input.view.package_of_file(&churn.file).map(|p| p.id.clone()).into_iter().collect(),
                    impact: dependents,
                    evidence: Evidence {
                        files: vec![churn.file.clone()],
                        git: Some(GitEvidence {
                            commits: Some(churn.commits),
                            last_change: Some(churn.last_change),
                            ..GitEvidence::default()
                        }),
                        ..Evidence::default()
                    },
                    spans: Vec::new(),
                }
            })
            .collect()
    }
}
