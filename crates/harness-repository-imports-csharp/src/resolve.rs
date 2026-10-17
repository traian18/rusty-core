//! Static resolution of C# `using` directives against the namespaces declared in the repository.
//!
//! A `using` names a namespace, not a file, so an edge points at the files that declare the
//! namespace (one file resolves directly, several are `Possible`). `using static` and aliases name
//! a type, which resolves to the file declaring it. `System` and `Microsoft` namespaces are
//! external, others are matched to declared packages by name.
use std::collections::{BTreeMap, HashSet};

use harness_workspace::{
    import_graph::{
        resolve_units, Decision, ImportBuildError, ImportFact, ImportReason, ImportResolver,
        ItemStatus, LanguageProfile, Resolution, ResolveContext, ResolveOutput, SourceUnit,
    },
    repository_intelligence::RootRelativePath,
};

use crate::extract::NAMESPACE_ITEM;

pub const RESOLVER_VERSION: u32 = 1;
const RULES: [&str; 6] = [
    "csharp.namespace_declaration",
    "csharp.type_declaration",
    "csharp.stdlib",
    "csharp.declared_package",
    "csharp.undeclared_external",
    "csharp.path_not_found",
];

#[derive(Clone, Copy, Debug, Default)]
pub struct CSharpImportResolver;

impl ImportResolver for CSharpImportResolver {
    fn id(&self) -> &'static str {
        "csharp-resolver"
    }

    fn version(&self) -> u32 {
        RESOLVER_VERSION
    }

    fn profiles(&self) -> Vec<LanguageProfile> {
        vec![LanguageProfile::CSharp]
    }

    fn rules(&self) -> Vec<&'static str> {
        RULES.to_vec()
    }

    fn unsupported(&self) -> Vec<&'static str> {
        vec![
            "edges are namespace-level: a using points at every file that declares the namespace, not at the types actually used",
            "implicit and global usings from the SDK, source generators, and references through types of the same namespace (no using is written)",
            "package names are matched to namespaces by prefix; packages whose namespaces differ from their id may be reported as undeclared",
        ]
    }

    fn resolve(&self, context: &ResolveContext<'_>) -> Result<ResolveOutput, ImportBuildError> {
        // Namespace -> files that declare it; "namespace.Type" -> files that declare the type.
        let mut namespaces: BTreeMap<String, Vec<&RootRelativePath>> = BTreeMap::new();
        let mut types: BTreeMap<String, Vec<&RootRelativePath>> = BTreeMap::new();
        for unit in context
            .units
            .iter()
            .filter(|u| u.profile == LanguageProfile::CSharp)
        {
            for item in &unit.extracted.items {
                if item.kind == NAMESPACE_ITEM {
                    let files = namespaces.entry(item.name.clone()).or_default();
                    if !files.contains(&&unit.path) {
                        files.push(&unit.path);
                    }
                } else if let Some(namespace) = item.scope.first().filter(|s| !s.is_empty()) {
                    types
                        .entry(format!("{namespace}.{}", item.name))
                        .or_default()
                        .push(&unit.path);
                }
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
        let max = context.limits.max_candidates;
        resolve_units(context, &[LanguageProfile::CSharp], |unit, fact| {
            decide(unit, fact, &namespaces, &types, &declared, max)
        })
    }
}

fn many(files: &[&RootRelativePath], max: usize) -> Resolution {
    match files {
        [only] => Resolution::ResolvedInternal {
            target_file: (*only).clone(),
            target_item: None,
            item_status: ItemStatus::NotAttempted,
            via: Vec::new(),
            definition_file: None,
        },
        files => Resolution::Possible {
            candidates: files.iter().take(max).map(|f| (*f).clone()).collect(),
            truncated: files.len() > max,
        },
    }
}

fn decide(
    unit: &SourceUnit,
    fact: &ImportFact,
    namespaces: &BTreeMap<String, Vec<&RootRelativePath>>,
    types: &BTreeMap<String, Vec<&RootRelativePath>>,
    declared: &BTreeMap<&str, HashSet<&str>>,
    max: usize,
) -> Decision {
    let name = fact.spelling.as_str();
    if let Some(files) = types.get(name) {
        return Decision::new(many(files, max), "csharp.type_declaration");
    }
    if let Some(files) = namespaces.get(name) {
        return Decision::new(many(files, max), "csharp.namespace_declaration");
    }
    let first = name.split('.').next().unwrap_or("");
    if matches!(first, "System" | "Microsoft" | "Windows")
        && !namespaces
            .keys()
            .any(|n| n == name || n.starts_with(&format!("{name}.")))
    {
        // Microsoft.* packages are declared like any other; prefer the declared package when there is one.
        let dependencies = unit
            .package
            .as_ref()
            .and_then(|p| declared.get(p.0.as_str()));
        if first == "Microsoft" {
            if let Some(package) = dependencies.and_then(|set| best_package(name, set)) {
                return Decision::new(
                    Resolution::DeclaredExternal {
                        package: package.to_owned(),
                    },
                    "csharp.declared_package",
                );
            }
        }
        return Decision::new(
            Resolution::DeclaredExternal {
                package: format!("stdlib:{first}"),
            },
            "csharp.stdlib",
        );
    }
    // A namespace of this repository with a missing sub-namespace or type.
    if namespaces
        .keys()
        .any(|n| name.starts_with(&format!("{n}.")))
    {
        return Decision::new(
            Resolution::Unresolved {
                reason: ImportReason::PathNotFound,
            },
            "csharp.path_not_found",
        );
    }
    let dependencies = unit
        .package
        .as_ref()
        .and_then(|p| declared.get(p.0.as_str()));
    match dependencies.and_then(|set| best_package(name, set)) {
        Some(package) => Decision::new(
            Resolution::DeclaredExternal {
                package: package.to_owned(),
            },
            "csharp.declared_package",
        ),
        None => Decision::new(
            Resolution::Unresolved {
                reason: ImportReason::UndeclaredExternal,
            },
            "csharp.undeclared_external",
        ),
    }
}

/// The declared package whose id is a prefix of the namespace (or the other way round), longest first.
fn best_package<'a>(namespace: &str, packages: &HashSet<&'a str>) -> Option<&'a str> {
    packages
        .iter()
        .filter(|id| !id.starts_with("project:"))
        .filter(|id| {
            let related =
                |long: &str, short: &str| long == short || long.starts_with(&format!("{short}."));
            related(namespace, id)
                || related(id, namespace)
                || (namespace.split('.').take(2).collect::<Vec<_>>()
                    == id.split('.').take(2).collect::<Vec<_>>()
                    && id.contains('.'))
        })
        .max_by_key(|id| (id.len(), std::cmp::Reverse(id.to_string())))
        .copied()
}
