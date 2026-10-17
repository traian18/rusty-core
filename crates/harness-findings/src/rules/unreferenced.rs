use std::collections::HashSet;

use harness_workspace::{
    framework_relations::RelationKind, repository_intelligence::RootRelativePath,
};

use super::{FindingRule, FindingsInput, Requirements};
use crate::model::{Confidence, Draft, Evidence, FindingKind, GitEvidence, Severity};

/// Directories whose files are entry points or support code rather than imported modules.
const ENTRY_DIRECTORIES: [&str; 9] = [
    "tests",
    "test",
    "__tests__",
    "examples",
    "benches",
    "bin",
    "scripts",
    "e2e",
    "fixtures",
];

/// An indexed file nothing imports and that is not an expected entry point.
pub struct UnreferencedFileRule;

impl FindingRule for UnreferencedFileRule {
    fn id(&self) -> &'static str {
        "unreferenced-file"
    }
    fn version(&self) -> u32 {
        2
    }
    fn kind(&self) -> FindingKind {
        FindingKind::Candidate
    }
    fn requires(&self) -> Requirements {
        // Git only raises confidence; the rule runs without it.
        Requirements::default()
    }
    fn description(&self) -> &'static str {
        "Candidate: indexed files with no resolved importers that are not roots, entry points, framework entry points, tests, or tooling files."
    }
    fn evaluate(&self, input: &FindingsInput<'_>) -> Vec<Draft> {
        let roots = declared_roots(input);
        let framework = framework_entry_points(input);
        input
            .repository
            .imports
            .files
            .iter()
            .filter(|file| input.view.incoming(&file.path).is_empty())
            .filter(|file| !roots.contains(&file.path) && !looks_like_entry_point(&file.path))
            .filter(|file| !framework.contains(&file.path))
            .filter(|file| !input.repository.imports.unattached.contains(&file.path))
            .map(|file| draft(input, &file.path))
            .collect()
    }
}

/// Files a framework reaches without an import: where a route or command is registered, and the
/// handler or command it points at, or a provider the framework instantiates. Nothing imports
/// them, so "unreferenced" would be wrong; the relation is their reference.
fn framework_entry_points(input: &FindingsInput<'_>) -> HashSet<RootRelativePath> {
    let mut files = HashSet::new();
    for relation in &input.repository.frameworks.relations {
        match relation.kind {
            RelationKind::RouteRegisteredTo | RelationKind::CommandExposedAs => {
                files.insert(relation.from.clone());
                files.extend(relation.to.clone());
            }
            RelationKind::ProviderInjectedInto => {
                files.insert(relation.from.clone());
            }
            RelationKind::ComponentRenders | RelationKind::ModelMappedToTable => {}
        }
    }
    files
}

fn declared_roots(input: &FindingsInput<'_>) -> HashSet<RootRelativePath> {
    input
        .repository
        .graph
        .packages
        .iter()
        .flat_map(|p| {
            p.facts
                .source_roots
                .iter()
                .chain(&p.facts.entry_points)
                .chain(&p.facts.target_roots)
        })
        .cloned()
        .collect()
}

/// Test, example, tooling, declaration, and conventional entry files.
fn looks_like_entry_point(path: &RootRelativePath) -> bool {
    let in_entry_directory = path
        .directory()
        .components()
        .any(|part| ENTRY_DIRECTORIES.contains(&part.as_os_str().to_string_lossy().as_ref()));
    let name = path
        .as_path()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    in_entry_directory
        || matches!(name.as_str(), "build.rs" | "lib.rs" | "main.rs" | "mod.rs")
        || name.starts_with("index.")
        || name.starts_with("main.")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.contains(".config.")
        || name.ends_with(".d.ts")
}

fn draft(input: &FindingsInput<'_>, file: &RootRelativePath) -> Draft {
    // Quiet history raises confidence: the file is unreferenced and was not touched in the window.
    let quiet = input
        .git
        .filter(|git| !git.coverage.shallow && git.coverage.commits_used > 0)
        .is_some_and(|git| {
            git.index
                .churn
                .binary_search_by(|c| c.file.cmp(file))
                .is_err()
        });
    Draft {
        key: file.as_path().display().to_string(),
        severity: Severity::Low,
        confidence: if quiet { Confidence::Medium } else { Confidence::Low },
        title: format!("{} may be unreferenced", file.as_path().display()),
        summary: "No resolved import in the repository targets this file, and it does not look like an entry point, test, or tooling file.".to_owned(),
        rationale: "It could be dead code, or it could be loaded by something this analysis cannot see (a bundler entry, a script, a dynamic path, a framework convention). Check how it is used before removing it.".to_owned(),
        subjects: vec![file.clone()],
        packages: input.view.package_of_file(file).map(|p| p.id.clone()).into_iter().collect(),
        impact: 1,
        evidence: Evidence {
            files: vec![file.clone()],
            git: quiet.then(GitEvidence::default).map(|mut git| {
                git.commits = Some(0);
                git
            }),
            ..Evidence::default()
        },
        spans: Vec::new(),
    }
}
