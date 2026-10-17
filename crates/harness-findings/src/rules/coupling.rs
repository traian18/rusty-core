use std::collections::{BTreeSet, HashMap};

use harness_git_analysis::CoChange;
use harness_workspace::repository_intelligence::RootRelativePath;

use super::{FindingRule, FindingsInput, Requirements};
use crate::{
    model::{Confidence, Draft, Evidence, FindingKind, GitEvidence, Severity},
    view::Walk,
};

/// Files that keep changing together although no import path connects them.
pub struct ImplicitCouplingRule;

impl FindingRule for ImplicitCouplingRule {
    fn id(&self) -> &'static str {
        "implicit-coupling"
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
        "Candidate: file pairs with strong co-change in history and no import path between them."
    }
    fn evaluate(&self, input: &FindingsInput<'_>) -> Vec<Draft> {
        let Some(git) = input.git else {
            return Vec::new();
        };
        let config = input.config;
        let strong = |c: &CoChange| {
            c.support >= config.implicit_min_support
                && c.confidence_per_mille >= config.implicit_min_confidence_per_mille
        };
        let by_pair: HashMap<(&RootRelativePath, &RootRelativePath), &CoChange> = git
            .index
            .co_changes
            .iter()
            .map(|c| ((&c.file, &c.partner), c))
            .collect();
        let mut reachable: HashMap<RootRelativePath, BTreeSet<RootRelativePath>> = HashMap::new();
        let mut connected = |from: &RootRelativePath, to: &RootRelativePath| {
            reachable
                .entry(from.clone())
                .or_insert_with(|| {
                    input
                        .view
                        .reachable(from, config.implicit_max_hops, Walk::Imports)
                })
                .contains(to)
        };
        let mut drafts = Vec::new();
        for forward in git
            .index
            .co_changes
            .iter()
            .filter(|c| c.file < c.partner && strong(c))
        {
            let Some(back) = by_pair
                .get(&(&forward.partner, &forward.file))
                .filter(|c| strong(c))
            else {
                continue;
            };
            let both_indexed =
                input.view.has_file(&forward.file) && input.view.has_file(&forward.partner);
            if !both_indexed
                || connected(&forward.file, &forward.partner)
                || connected(&forward.partner, &forward.file)
            {
                continue;
            }
            let confidence = forward.confidence_per_mille.min(back.confidence_per_mille);
            let strongest = forward.support >= 10 && confidence >= 800;
            let files = vec![forward.file.clone(), forward.partner.clone()];
            drafts.push(Draft {
                key: format!("{}|{}", forward.file.as_path().display(), forward.partner.as_path().display()),
                severity: Severity::Info,
                confidence: if strongest { Confidence::High } else { Confidence::Medium },
                title: format!("{} and {} change together without importing each other", forward.file.as_path().display(), forward.partner.as_path().display()),
                summary: format!(
                    "They changed in the same {} commits ({}\u{2030} of the first file's commits, {}\u{2030} of the second's), and no import path joins them within {} hops.",
                    forward.support, forward.confidence_per_mille, back.confidence_per_mille, config.implicit_max_hops
                ),
                rationale: "Files that change together often share a hidden contract (a format, a protocol, duplicated logic). Consider making the relationship explicit, or confirm it is coincidence such as shared release edits.".to_owned(),
                subjects: files.clone(),
                packages: files.iter().filter_map(|f| input.view.package_of_file(f)).map(|p| p.id.clone()).collect::<BTreeSet<_>>().into_iter().collect(),
                impact: 2,
                evidence: Evidence {
                    files,
                    git: Some(GitEvidence {
                        support: Some(forward.support),
                        confidence_per_mille: Some(confidence),
                        commits: Some(forward.file_commits),
                        last_change: None,
                        example_commits: forward.example_commits.clone(),
                    }),
                    ..Evidence::default()
                },
                spans: Vec::new(),
            });
        }
        drafts
    }
}
