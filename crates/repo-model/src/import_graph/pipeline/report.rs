//! What a build noticed along the way, and how facts become edges.

use std::collections::{HashMap, HashSet};

use crate::import_graph::{
    index::IndexedFile,
    model::{
        ExtractedFile, ImportEdge, ImportFact, ImportFactId, LanguageProfile, ResolveOutput,
        SourceUnit,
    },
};
use crate::repository_intelligence::{
    DiscoveryDiagnostic, DiscoveryError, PackageId, RootRelativePath,
};

/// Problems collected while building one index.
#[derive(Default)]
pub(super) struct BuildReport {
    pub(super) diagnostics: Vec<DiscoveryDiagnostic>,
    pub(super) unattached: Vec<RootRelativePath>,
    pub(super) partial: bool,
}

/// Extracted sources and the index rows describing them.
pub(super) struct Extraction {
    pub(super) units: Vec<SourceUnit>,
    pub(super) files: Vec<IndexedFile>,
    pub(super) bytes: u64,
}

pub(super) fn note_extraction_issues(
    path: &RootRelativePath,
    extracted: &ExtractedFile,
    report: &mut BuildReport,
) {
    if extracted.truncated {
        report.omit(path, "file exceeded the per-file import fact limit");
    }
    report.diagnostics.extend(
        extracted
            .diagnostics
            .iter()
            .map(|message| DiscoveryDiagnostic::for_path(path, message.clone())),
    );
}

pub(super) fn facts_for<'a>(
    units: &'a [SourceUnit],
    profiles: &[LanguageProfile],
) -> HashMap<&'a ImportFactId, &'a ImportFact> {
    units
        .iter()
        .filter(|unit| profiles.contains(&unit.profile))
        .flat_map(|unit| unit.extracted.facts.iter())
        .map(|fact| (&fact.id, fact))
        .collect()
}

/// Pairs every resolved fact with its package attribution; each fact must be answered exactly once.
pub(super) fn attribute_edges(
    resolver: &str,
    output: ResolveOutput,
    facts: &HashMap<&ImportFactId, &ImportFact>,
    packages: &HashMap<&RootRelativePath, &Option<PackageId>>,
    report: &mut BuildReport,
) -> Result<Vec<ImportEdge>, DiscoveryError> {
    let failure = |what: &str| DiscoveryError::Failed(format!("resolver {resolver} {what}"));
    let mut seen = HashSet::new();
    let mut edges = Vec::new();
    for resolved in output.resolved {
        let fact = facts
            .get(&resolved.fact)
            .ok_or_else(|| failure("returned an unknown fact"))?;
        if !seen.insert(resolved.fact.clone()) {
            return Err(failure("returned a fact twice"));
        }
        let package_of = |file: &RootRelativePath| packages.get(file).and_then(|p| (*p).clone());
        edges.push(ImportEdge {
            from_package: package_of(&fact.importing_file),
            to_package: resolved.resolution.target_file().and_then(package_of),
            fact: (*fact).clone(),
            resolution: resolved.resolution,
            rule: resolved.rule,
            evidence: resolved.evidence,
        });
    }
    if seen.len() != facts.len() {
        return Err(failure("did not resolve every fact"));
    }
    report.diagnostics.extend(output.diagnostics);
    report.unattached.extend(output.unattached);
    Ok(edges)
}

impl BuildReport {
    /// Records a diagnostic and marks coverage as incomplete.
    pub(super) fn omit(&mut self, path: &RootRelativePath, message: impl Into<String>) {
        self.diagnostics
            .push(DiscoveryDiagnostic::for_path(path, message));
        self.partial = true;
    }
}
