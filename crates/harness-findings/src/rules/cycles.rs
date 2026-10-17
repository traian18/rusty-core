use std::collections::BTreeSet;

use harness_workspace::dependency_groups::GroupKind;

use super::{bounded, FindingRule, FindingsInput, Requirements};
use crate::model::{Confidence, Draft, Evidence, FindingKind, Severity, SourceSpan};

/// Runtime import cycles from the dependency groups.
pub struct ImportCycleRule;
/// Cycles that exist only once type-only imports are counted.
pub struct TypeOnlyImportCycleRule;

impl FindingRule for ImportCycleRule {
    fn id(&self) -> &'static str {
        "import-cycle"
    }
    fn version(&self) -> u32 {
        1
    }
    fn kind(&self) -> FindingKind {
        FindingKind::Measurable
    }
    fn requires(&self) -> Requirements {
        Requirements {
            groups: true,
            git: false,
        }
    }
    fn description(&self) -> &'static str {
        "Files that are mutually reachable through resolved runtime imports."
    }
    fn evaluate(&self, input: &FindingsInput<'_>) -> Vec<Draft> {
        cycle_drafts(input, false)
    }
}

impl FindingRule for TypeOnlyImportCycleRule {
    fn id(&self) -> &'static str {
        "type-only-import-cycle"
    }
    fn version(&self) -> u32 {
        1
    }
    fn kind(&self) -> FindingKind {
        FindingKind::Measurable
    }
    fn requires(&self) -> Requirements {
        Requirements {
            groups: true,
            git: false,
        }
    }
    fn description(&self) -> &'static str {
        "Files that are mutually reachable only when type-only imports are counted."
    }
    fn evaluate(&self, input: &FindingsInput<'_>) -> Vec<Draft> {
        cycle_drafts(input, true)
    }
}

fn cycle_drafts(input: &FindingsInput<'_>, type_only: bool) -> Vec<Draft> {
    let confidence = if input.repository.imports.partial {
        Confidence::Medium
    } else {
        Confidence::High
    };
    input
        .repository
        .groups
        .groups
        .iter()
        .filter(|group| group.kind == GroupKind::ImportCycle && group.involves_type_only == type_only)
        .map(|group| {
            let packages = group
                .members
                .iter()
                .filter_map(|file| input.view.package_of_file(file))
                .map(|package| package.id.clone())
                .collect::<BTreeSet<_>>();
            let severity = match (type_only, packages.len() > 1) {
                (true, _) => Severity::Info,
                (false, true) => Severity::High,
                (false, false) => Severity::Medium,
            };
            let spans = group
                .cycle_sites
                .iter()
                .filter_map(|fact| input.view.edge(fact))
                .map(|edge| SourceSpan {
                    file: edge.fact.importing_file.clone(),
                    start: edge.fact.span.start,
                    end: edge.fact.span.end,
                })
                .collect::<Vec<_>>();
            let (spans, spans_truncated) = bounded(spans, input.config.max_evidence_per_finding);
            let (sites, sites_truncated) = bounded(group.cycle_sites.clone(), input.config.max_evidence_per_finding);
            let first = group.members[0].as_path().display();
            Draft {
                key: group.id.0.trim_start_matches("import_cycle:").to_owned(),
                severity,
                confidence,
                title: format!("{} between {} files", if type_only { "Type-only import cycle" } else { "Import cycle" }, group.members.len()),
                summary: format!(
                    "{} files, starting at {first}, import each other {}.",
                    group.members.len(),
                    if type_only { "only through type-only imports" } else { "directly or indirectly" }
                ),
                rationale: if type_only {
                    "The cycle is erased at runtime, so it cannot fail at load time, but it still couples these files' types; consider moving shared types to a file both import.".to_owned()
                } else {
                    "Files that import each other can only be loaded, tested and changed together, and runtime cycles can fail at load time; consider extracting what they share, or inverting one dependency.".to_owned()
                },
                subjects: group.members.clone(),
                packages: packages.into_iter().collect(),
                impact: group.members.len(),
                evidence: Evidence {
                    files: group.members.clone(),
                    import_sites: sites,
                    groups: vec![group.id.clone()],
                    truncated: spans_truncated || sites_truncated || group.evidence_truncated,
                    ..Evidence::default()
                },
                spans,
            }
        })
        .collect()
}
