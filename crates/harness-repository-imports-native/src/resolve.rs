//! Static resolution of C and C++ includes.
//!
//! A quoted include is looked up next to the including file, then in the include directories of
//! its package (from CMake or a compile database), then in the package directory. An angle include
//! is looked up in the include directories only; one that is not found is a standard or system
//! header, or an external library matched by name to a declared dependency. No preprocessor runs.
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use harness_workspace::{
    import_graph::{
        resolve_units, Decision, ImportBuildError, ImportFact, ImportReason, ImportResolver,
        ItemStatus, LanguageProfile, Resolution, ResolveContext, ResolveOutput, SourceUnit,
    },
    repository_intelligence::RootRelativePath,
};

use crate::{headers::is_system_header, manifest::normalize};

pub const RESOLVER_VERSION: u32 = 1;
const RULES: [&str; 7] = [
    "native.quoted_relative_to_file",
    "native.include_directory",
    "native.package_directory",
    "native.standard_header",
    "native.declared_library",
    "native.undeclared_external",
    "native.dynamic_include",
];

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeImportResolver;

impl ImportResolver for NativeImportResolver {
    fn id(&self) -> &'static str {
        "c-cpp-resolver"
    }

    fn version(&self) -> u32 {
        RESOLVER_VERSION
    }

    fn profiles(&self) -> Vec<LanguageProfile> {
        vec![LanguageProfile::C, LanguageProfile::Cpp]
    }

    fn rules(&self) -> Vec<&'static str> {
        RULES.to_vec()
    }

    fn unsupported(&self) -> Vec<&'static str> {
        vec![
            "includes built by macros (#include MACRO) and conditional compilation: every branch is read, none is evaluated",
            "per-target and per-file compile flags that are not in CMake's include_directories calls or compile_commands.json; generated headers",
            "C++20 modules (import) and Makefile or Bazel build files",
            "the include graph is not a call graph: a header and its implementation file are not linked",
        ]
    }

    fn resolve(&self, context: &ResolveContext<'_>) -> Result<ResolveOutput, ImportBuildError> {
        let files: HashSet<&RootRelativePath> = context
            .units
            .iter()
            .filter(|u| matches!(u.profile, LanguageProfile::C | LanguageProfile::Cpp))
            .map(|u| &u.path)
            .collect();
        // Package id -> (directory, include directories, declared dependency names).
        let packages: BTreeMap<&str, (PathBuf, Vec<PathBuf>, HashSet<String>)> = context
            .structure
            .packages
            .iter()
            .filter(|p| {
                p.manifest
                    .as_path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n == "CMakeLists.txt" || n == "compile_commands.json")
            })
            .map(|p| {
                let includes = p
                    .facts
                    .inferred_facts
                    .iter()
                    .filter_map(|f| f.strip_prefix("include:"))
                    .map(PathBuf::from)
                    .collect();
                let declared = p
                    .facts
                    .declared_dependencies
                    .iter()
                    .map(|d| d.to_lowercase())
                    .collect();
                (
                    p.id.0.as_str(),
                    (p.manifest.directory().to_path_buf(), includes, declared),
                )
            })
            .collect();
        let max = context.limits.max_candidates;
        resolve_units(
            context,
            &[LanguageProfile::C, LanguageProfile::Cpp],
            |unit, fact| decide(unit, fact, &files, &packages, max),
        )
    }
}

fn find(
    files: &HashSet<&RootRelativePath>,
    directory: &Path,
    header: &str,
) -> Option<RootRelativePath> {
    let joined = directory.join(header);
    normalize(&joined)
        .and_then(|p| RootRelativePath::new(p).ok())
        .filter(|p| files.contains(p))
}

fn found(target: RootRelativePath) -> Resolution {
    Resolution::ResolvedInternal {
        target_file: target,
        target_item: None,
        item_status: ItemStatus::NotAttempted,
        via: Vec::new(),
        definition_file: None,
    }
}

fn decide(
    unit: &SourceUnit,
    fact: &ImportFact,
    files: &HashSet<&RootRelativePath>,
    packages: &BTreeMap<&str, (PathBuf, Vec<PathBuf>, HashSet<String>)>,
    _max: usize,
) -> Decision {
    if !fact.literal_specifier {
        return Decision::new(
            Resolution::DynamicOrUnsupported {
                reason: ImportReason::NonLiteralSpecifier,
            },
            "native.dynamic_include",
        );
    }
    let header = fact.spelling.as_str();
    let system = fact.scope.iter().any(|s| s == "system");
    let package = unit
        .package
        .as_ref()
        .and_then(|p| packages.get(p.0.as_str()));
    if !system {
        if let Some(target) = find(files, unit.path.directory(), header) {
            return Decision::new(found(target), "native.quoted_relative_to_file");
        }
    }
    if let Some((directory, includes, _)) = package {
        for include in includes {
            if let Some(target) = find(files, include, header) {
                return Decision::new(found(target), "native.include_directory");
            }
        }
        if !system {
            if let Some(target) = find(files, directory, header) {
                return Decision::new(found(target), "native.package_directory");
            }
        }
    }
    if is_system_header(header) {
        return Decision::new(
            Resolution::DeclaredExternal {
                package: format!("stdlib:{header}"),
            },
            "native.standard_header",
        );
    }
    let first = header.split('/').next().unwrap_or("").to_lowercase();
    let stem = first.split('.').next().unwrap_or("").to_owned();
    if let Some((_, _, declared)) = package {
        if let Some(library) = declared.iter().find(|d| {
            **d == first
                || **d == stem
                || d.starts_with(&format!("{stem}_"))
                || d.trim_start_matches("lib") == stem
        }) {
            return Decision::new(
                Resolution::DeclaredExternal {
                    package: library.clone(),
                },
                "native.declared_library",
            );
        }
    }
    Decision::new(
        Resolution::Unresolved {
            reason: ImportReason::UndeclaredExternal,
        },
        "native.undeclared_external",
    )
}
