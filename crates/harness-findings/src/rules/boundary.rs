use std::collections::BTreeMap;

use harness_workspace::{import_graph::ImportEdge, repository_intelligence::RootRelativePath};

use super::{bounded, FindingRule, FindingsInput};
use crate::{
    config::{BoundaryRule, Selector},
    model::{Confidence, Draft, Evidence, FindingKind, SourceSpan},
    view::RepoView,
};

/// Imports that break a configured forbidden dependency. Reports nothing without a policy.
pub struct BoundaryViolationRule;

impl FindingRule for BoundaryViolationRule {
    fn id(&self) -> &'static str {
        "boundary-violation"
    }
    fn version(&self) -> u32 {
        1
    }
    fn kind(&self) -> FindingKind {
        FindingKind::Measurable
    }
    fn description(&self) -> &'static str {
        "Resolved imports that match a forbidden dependency in the configured boundary policy."
    }
    fn evaluate(&self, input: &FindingsInput<'_>) -> Vec<Draft> {
        let mut drafts = Vec::new();
        for rule in &input.config.policy.rules {
            let mut by_file: BTreeMap<&RootRelativePath, Vec<&ImportEdge>> = BTreeMap::new();
            for edge in &input.repository.imports.edges {
                let Some(target) = edge.resolution.target_file() else {
                    continue;
                };
                if selects(&rule.from, &edge.fact.importing_file, input.view)
                    && selects(&rule.to, target, input.view)
                {
                    by_file
                        .entry(&edge.fact.importing_file)
                        .or_default()
                        .push(edge);
                }
            }
            drafts.extend(
                by_file
                    .into_iter()
                    .map(|(file, edges)| draft(input, rule, file, &edges)),
            );
        }
        drafts
    }
}

fn selects(selector: &Selector, file: &RootRelativePath, view: &RepoView<'_>) -> bool {
    match selector {
        Selector::Package(name) => view
            .package_of_file(file)
            .is_some_and(|package| package.name == *name),
        Selector::PathPrefix(prefix) => file.as_path().starts_with(prefix.as_path()),
    }
}

fn draft(
    input: &FindingsInput<'_>,
    rule: &BoundaryRule,
    file: &RootRelativePath,
    edges: &[&ImportEdge],
) -> Draft {
    let spans = edges
        .iter()
        .map(|edge| SourceSpan {
            file: file.clone(),
            start: edge.fact.span.start,
            end: edge.fact.span.end,
        })
        .collect::<Vec<_>>();
    let (spans, spans_truncated) = bounded(spans, input.config.max_evidence_per_finding);
    let (sites, sites_truncated) = bounded(
        edges.iter().map(|e| e.fact.id.clone()).collect(),
        input.config.max_evidence_per_finding,
    );
    let mut targets = edges
        .iter()
        .filter_map(|e| e.resolution.target_file().cloned())
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();
    let mut subjects = vec![file.clone()];
    subjects.extend(targets);
    Draft {
        key: format!("{}|{}", rule.id, file.as_path().display()),
        severity: rule.severity,
        confidence: Confidence::High,
        title: format!(
            "{} breaks boundary rule `{}`",
            file.as_path().display(),
            rule.id
        ),
        summary: format!(
            "{} import site(s) match the forbidden dependency `{}`.",
            edges.len(),
            rule.id
        ),
        rationale: rule.reason.clone(),
        impact: subjects.len(),
        packages: input
            .view
            .package_of_file(file)
            .map(|p| p.id.clone())
            .into_iter()
            .collect(),
        evidence: Evidence {
            files: subjects.clone(),
            import_sites: sites,
            truncated: spans_truncated || sites_truncated,
            ..Evidence::default()
        },
        subjects,
        spans,
    }
}
