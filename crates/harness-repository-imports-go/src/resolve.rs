//! Static resolution of Go import paths.
//!
//! A Go package is a directory, so an import resolves to the directory of a module found in the
//! repository (by its `go.mod` module path) and the edge points at that package's lead file.
//! Standard library paths (no dot in the first element) and declared modules are external.
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use harness_workspace::{
    import_graph::{
        resolve_units, Decision, ImportBuildError, ImportReason, ImportResolver, ItemStatus,
        LanguageProfile, Resolution, ResolveContext, ResolveOutput, SourceUnit,
    },
    repository_intelligence::RootRelativePath,
};

pub const RESOLVER_VERSION: u32 = 1;
const RULES: [&str; 5] = [
    "go.module_package_directory",
    "go.stdlib",
    "go.declared_module",
    "go.undeclared_external",
    "go.path_not_found",
];

#[derive(Clone, Copy, Debug, Default)]
pub struct GoImportResolver;

impl ImportResolver for GoImportResolver {
    fn id(&self) -> &'static str {
        "go-resolver"
    }

    fn version(&self) -> u32 {
        RESOLVER_VERSION
    }

    fn profiles(&self) -> Vec<LanguageProfile> {
        vec![LanguageProfile::Go]
    }

    fn rules(&self) -> Vec<&'static str> {
        RULES.to_vec()
    }

    fn unsupported(&self) -> Vec<&'static str> {
        vec![
            "replace directives to local paths, go.work workspaces and vendored modules",
            "build tags, cgo (`import \"C\"`) and per-platform files",
            "an edge points at the lead file of an imported package (the file named like the directory, then doc.go, then the first); which file inside it is used is not known",
        ]
    }

    fn resolve(&self, context: &ResolveContext<'_>) -> Result<ResolveOutput, ImportBuildError> {
        // Module path -> its root directory, from go.mod packages (the package name is the module path).
        let modules: Vec<(String, PathBuf, &RootRelativePath)> = context
            .structure
            .packages
            .iter()
            .filter(|p| {
                p.manifest
                    .as_path()
                    .file_name()
                    .is_some_and(|n| n == "go.mod")
            })
            .map(|p| {
                (
                    p.name.clone(),
                    p.manifest.directory().to_path_buf(),
                    &p.manifest,
                )
            })
            .collect();
        // Directory -> the Go files in it (tests excluded), sorted.
        let mut directories: BTreeMap<PathBuf, Vec<&RootRelativePath>> = BTreeMap::new();
        for unit in context
            .units
            .iter()
            .filter(|u| u.profile == LanguageProfile::Go)
        {
            let name = unit
                .path
                .as_path()
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            if !name.ends_with("_test.go") {
                directories
                    .entry(unit.path.directory().to_path_buf())
                    .or_default()
                    .push(&unit.path);
            }
        }
        let declared: BTreeMap<&str, HashSet<&str>> = context
            .structure
            .packages
            .iter()
            .map(|p| {
                (
                    p.id.0.as_str(),
                    p.facts
                        .declared_dependencies
                        .iter()
                        .map(String::as_str)
                        .collect(),
                )
            })
            .collect();
        resolve_units(context, &[LanguageProfile::Go], |unit, fact| {
            decide(unit, &fact.spelling, &modules, &directories, &declared)
        })
    }
}

/// The module that owns an import path: the longest module path that is a prefix at a `/` boundary.
fn owner<'a>(
    path: &str,
    modules: &'a [(String, PathBuf, &'a RootRelativePath)],
) -> Option<&'a (String, PathBuf, &'a RootRelativePath)> {
    modules
        .iter()
        .filter(|(module, _, _)| {
            path == module
                || path
                    .strip_prefix(module.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(|(module, _, _)| module.len())
}

/// The lead file of a package directory: `<dir>.go`, then `doc.go`, then the first file.
fn lead<'a>(
    directory: &std::path::Path,
    files: &[&'a RootRelativePath],
) -> Option<&'a RootRelativePath> {
    let stem = directory.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let named = |wanted: &str| {
        files
            .iter()
            .find(|f| f.as_path().file_name().and_then(|n| n.to_str()) == Some(wanted))
            .copied()
    };
    named(&format!("{stem}.go"))
        .or_else(|| named("doc.go"))
        .or_else(|| files.first().copied())
}

fn decide(
    unit: &SourceUnit,
    path: &str,
    modules: &[(String, PathBuf, &RootRelativePath)],
    directories: &BTreeMap<PathBuf, Vec<&RootRelativePath>>,
    declared: &BTreeMap<&str, HashSet<&str>>,
) -> Decision {
    if let Some((module, root, manifest)) = owner(path, modules) {
        let rest = path
            .strip_prefix(module.as_str())
            .unwrap_or("")
            .trim_start_matches('/');
        let directory = if rest.is_empty() {
            root.clone()
        } else {
            root.join(rest)
        };
        return match directories
            .get(&directory)
            .and_then(|files| lead(&directory, files))
        {
            Some(file) => {
                let mut decision = Decision::new(
                    Resolution::ResolvedInternal {
                        target_file: file.clone(),
                        target_item: None,
                        item_status: ItemStatus::NotAttempted,
                        via: Vec::new(),
                        definition_file: None,
                    },
                    "go.module_package_directory",
                );
                decision.evidence.push((*manifest).clone());
                decision
            }
            None => Decision::new(
                Resolution::Unresolved {
                    reason: ImportReason::PathNotFound,
                },
                "go.path_not_found",
            ),
        };
    }
    let first = path.split('/').next().unwrap_or("");
    if !first.contains('.') {
        return Decision::new(
            Resolution::DeclaredExternal {
                package: format!("stdlib:{path}"),
            },
            "go.stdlib",
        );
    }
    let dependencies = unit
        .package
        .as_ref()
        .and_then(|p| declared.get(p.0.as_str()));
    let module = dependencies.and_then(|set| {
        set.iter()
            .filter(|m| {
                path == **m
                    || path
                        .strip_prefix(**m)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .max_by_key(|m| m.len())
    });
    match module {
        Some(module) => Decision::new(
            Resolution::DeclaredExternal {
                package: (*module).to_owned(),
            },
            "go.declared_module",
        ),
        None => Decision::new(
            Resolution::Unresolved {
                reason: ImportReason::UndeclaredExternal,
            },
            "go.undeclared_external",
        ),
    }
}
