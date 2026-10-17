use std::collections::BTreeSet;

use harness_workspace::import_graph::Resolution;

use super::{normalize, FindingRule, FindingsInput};
use crate::model::{Confidence, Draft, Evidence, FindingKind, Severity};

/// A regular dependency a package declares but none of its indexed files import.
pub struct UnusedDeclaredDependencyRule;

impl FindingRule for UnusedDeclaredDependencyRule {
    fn id(&self) -> &'static str {
        "unused-declared-dependency"
    }
    fn version(&self) -> u32 {
        1
    }
    fn kind(&self) -> FindingKind {
        FindingKind::Candidate
    }
    fn description(&self) -> &'static str {
        "Candidate: dependencies declared by a package's manifest (not dev, build, peer, or type stubs) that no file imports."
    }
    fn evaluate(&self, input: &FindingsInput<'_>) -> Vec<Draft> {
        let mut drafts = Vec::new();
        for package in &input.repository.graph.packages {
            let owned = input
                .repository
                .imports
                .files
                .iter()
                .filter(|file| file.package.as_ref() == Some(&package.id))
                .map(|file| &file.path)
                .collect::<BTreeSet<_>>();
            if owned.is_empty() {
                continue;
            }
            let used = used_names(input, &owned);
            for name in package
                .facts
                .declared_dependencies
                .iter()
                .filter(|d| {
                    !package.facts.dev_dependencies.contains(d) && !d.starts_with("@types/")
                })
                .filter(|d| !used.contains(&normalize(d)))
            {
                drafts.push(Draft {
                    key: format!("{}|{name}", package.id.0),
                    severity: Severity::Low,
                    confidence: Confidence::Low,
                    title: format!("{} declares `{name}` but no file imports it", package.name),
                    summary: format!("None of {}'s {} indexed files imports `{name}`.", package.name, owned.len()),
                    rationale: "It may be unused, or used in ways imports do not show: a procedural macro or derive, a build or runtime plugin, a CLI tool, or a peer requirement. Check before removing it.".to_owned(),
                    subjects: vec![package.manifest.clone()],
                    packages: vec![package.id.clone()],
                    impact: 1,
                    evidence: Evidence { manifests: vec![package.manifest.clone()], ..Evidence::default() },
                    spans: Vec::new(),
                });
            }
        }
        drafts
    }
}

/// Normalised names of every external package or workspace package the files import.
fn used_names(
    input: &FindingsInput<'_>,
    owned: &BTreeSet<&harness_workspace::repository_intelligence::RootRelativePath>,
) -> BTreeSet<String> {
    let mut used = BTreeSet::new();
    for edge in &input.repository.imports.edges {
        if !owned.contains(&edge.fact.importing_file) {
            continue;
        }
        match &edge.resolution {
            Resolution::DeclaredExternal { package } => {
                used.insert(normalize(package));
            }
            Resolution::ResolvedInternal { .. } => {
                if let Some(target) = edge
                    .to_package
                    .as_ref()
                    .and_then(|id| input.view.package(id))
                {
                    used.insert(normalize(&target.name));
                }
            }
            _ => {}
        }
    }
    used
}
