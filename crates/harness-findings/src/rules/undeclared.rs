use std::collections::BTreeMap;

use harness_workspace::{
    import_graph::{ImportEdge, ImportReason, LanguageProfile, Resolution},
    repository_intelligence::{PackageId, RootRelativePath},
};

use super::{bounded, normalize, FindingRule, FindingsInput};
use crate::model::{Confidence, Draft, Evidence, FindingKind, Severity, SourceSpan};

/// A bare import of a package that no manifest of the importing package declares.
pub struct UndeclaredDependencyRule;
/// A resolved import into another workspace package that the importing manifest does not declare.
pub struct UndeclaredWorkspaceDependencyRule;

/// The package an import names, or `None` for forms that are not package names.
fn external_name(edge: &ImportEdge) -> Option<String> {
    let spelling = edge.fact.spelling.as_str();
    match edge.fact.profile {
        LanguageProfile::Rust => {
            let first = spelling.split("::").next()?;
            // Crate names are snake_case; CamelCase names are types or variants in scope.
            let is_crate_like = first.starts_with(|c: char| c.is_ascii_lowercase() || c == '_');
            (is_crate_like && !matches!(first, "crate" | "self" | "super"))
                .then(|| first.to_owned())
        }
        LanguageProfile::TypeScript | LanguageProfile::JavaScript => {
            let mut parts = spelling.split('/');
            let first = parts.next()?;
            if first.starts_with('@') {
                Some(format!("{first}/{}", parts.next()?))
            } else {
                Some(first.to_owned())
            }
        }
        // No resolver reports undeclared packages for these yet, so there is no name to give.
        LanguageProfile::Python
        | LanguageProfile::Go
        | LanguageProfile::Java
        | LanguageProfile::CSharp
        | LanguageProfile::Php
        | LanguageProfile::C
        | LanguageProfile::Cpp => None,
    }
}

impl FindingRule for UndeclaredDependencyRule {
    fn id(&self) -> &'static str {
        "undeclared-dependency"
    }
    fn version(&self) -> u32 {
        1
    }
    fn kind(&self) -> FindingKind {
        FindingKind::Measurable
    }
    fn description(&self) -> &'static str {
        "Imports of packages that the importing package's manifest does not declare."
    }
    fn evaluate(&self, input: &FindingsInput<'_>) -> Vec<Draft> {
        let mut grouped: BTreeMap<(PackageId, String), Vec<&ImportEdge>> = BTreeMap::new();
        for edge in &input.repository.imports.edges {
            let undeclared = matches!(
                edge.resolution,
                Resolution::Unresolved {
                    reason: ImportReason::UndeclaredExternal
                }
            );
            if let (true, Some(package), Some(name)) =
                (undeclared, &edge.from_package, external_name(edge))
            {
                grouped
                    .entry((package.clone(), name))
                    .or_default()
                    .push(edge);
            }
        }
        grouped
            .into_iter()
            .map(|((package, name), edges)| {
                let manifest = input.view.package(&package).map(|p| p.manifest.clone());
                let files = unique_files(&edges);
                let spans = edges.iter().map(|e| span_of(e)).collect::<Vec<_>>();
                let (spans, spans_truncated) = bounded(spans, input.config.max_evidence_per_finding);
                let (sites, sites_truncated) = bounded(edges.iter().map(|e| e.fact.id.clone()).collect(), input.config.max_evidence_per_finding);
                let label = input.view.package(&package).map_or(package.0.clone(), |p| p.name.clone());
                Draft {
                    key: format!("{}|{name}", package.0),
                    severity: Severity::Medium,
                    confidence: Confidence::Medium,
                    title: format!("`{name}` is imported but not declared by {label}"),
                    summary: format!("{} import site(s) in {} file(s) use `{name}`, which is not in {label}'s manifest.", edges.len(), files.len()),
                    rationale: "A dependency that works only because another package happens to provide it breaks when that changes; declare it in the manifest that imports it. If the name is provided by tooling, hoisting or a path alias, suppress this finding with a reason.".to_owned(),
                    impact: files.len(),
                    subjects: files.clone(),
                    packages: vec![package],
                    evidence: Evidence {
                        files,
                        import_sites: sites,
                        manifests: manifest.into_iter().collect(),
                        truncated: spans_truncated || sites_truncated,
                        ..Evidence::default()
                    },
                    spans,
                }
            })
            .collect()
    }
}

impl FindingRule for UndeclaredWorkspaceDependencyRule {
    fn id(&self) -> &'static str {
        "undeclared-workspace-dependency"
    }
    fn version(&self) -> u32 {
        1
    }
    fn kind(&self) -> FindingKind {
        FindingKind::Measurable
    }
    fn description(&self) -> &'static str {
        "Resolved imports into another workspace package that the importing manifest does not declare."
    }
    fn evaluate(&self, input: &FindingsInput<'_>) -> Vec<Draft> {
        let mut grouped: BTreeMap<(PackageId, PackageId), Vec<&ImportEdge>> = BTreeMap::new();
        for edge in &input.repository.imports.edges {
            if let (Some(from), Some(to)) = (&edge.from_package, &edge.to_package) {
                if from != to && declares_nothing_for(input, from, to) {
                    grouped
                        .entry((from.clone(), to.clone()))
                        .or_default()
                        .push(edge);
                }
            }
        }
        grouped
            .into_iter()
            .map(|((from, to), edges)| {
                let (from_pkg, to_pkg) = (input.view.package(&from), input.view.package(&to));
                let names = (
                    from_pkg.map_or(from.0.clone(), |p| p.name.clone()),
                    to_pkg.map_or(to.0.clone(), |p| p.name.clone()),
                );
                let files = unique_files(&edges);
                let spans = edges.iter().map(|e| span_of(e)).collect::<Vec<_>>();
                let (spans, spans_truncated) = bounded(spans, input.config.max_evidence_per_finding);
                let (sites, sites_truncated) = bounded(edges.iter().map(|e| e.fact.id.clone()).collect(), input.config.max_evidence_per_finding);
                Draft {
                    key: format!("{}->{}", from.0, to.0),
                    severity: Severity::Medium,
                    confidence: Confidence::High,
                    title: format!("{} imports {} without declaring it", names.0, names.1),
                    summary: format!("{} import site(s) reach into the workspace package {}, which {}'s manifest does not list.", edges.len(), names.1, names.0),
                    rationale: "An import that resolves only through a path alias or layout accident bypasses the package graph, so builds, publishing and ordering tools do not see the dependency; declare it, or route the import through the package's entry point.".to_owned(),
                    impact: files.len(),
                    subjects: files.clone(),
                    packages: vec![from, to],
                    evidence: Evidence {
                        files,
                        import_sites: sites,
                        manifests: from_pkg.map(|p| p.manifest.clone()).into_iter().collect(),
                        truncated: spans_truncated || sites_truncated,
                        ..Evidence::default()
                    },
                    spans,
                }
            })
            .collect()
    }
}

/// True when the importing package's manifest lists neither the target's name nor a normalised form of it.
fn declares_nothing_for(input: &FindingsInput<'_>, from: &PackageId, to: &PackageId) -> bool {
    let (Some(from_pkg), Some(to_pkg)) = (input.view.package(from), input.view.package(to)) else {
        return false;
    };
    let same_ecosystem =
        from_pkg.manifest.as_path().file_name() == to_pkg.manifest.as_path().file_name();
    let target = normalize(&to_pkg.name);
    same_ecosystem
        && !from_pkg
            .facts
            .declared_dependencies
            .iter()
            .any(|dep| normalize(dep) == target)
}

fn unique_files(edges: &[&ImportEdge]) -> Vec<RootRelativePath> {
    let mut files = edges
        .iter()
        .map(|e| e.fact.importing_file.clone())
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    files
}

fn span_of(edge: &ImportEdge) -> SourceSpan {
    SourceSpan {
        file: edge.fact.importing_file.clone(),
        start: edge.fact.span.start,
        end: edge.fact.span.end,
    }
}
