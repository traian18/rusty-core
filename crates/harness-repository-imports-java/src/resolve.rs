//! Static resolution of Java and Kotlin imports against the declarations in the repository.
//!
//! Java and Kotlin files are indexed together, so a Kotlin file importing a Java class (or the
//! other way round) resolves like an import within one language. Kotlin also imports top-level
//! functions, properties and type aliases, which its files declare as items.
//!
//! Java maps a package to a directory and a public type to a file, but the match is made on the
//! `package` declaration, so any source layout works. Standard library packages are external; other
//! packages are matched to declared dependencies by their group id (sharing the leading segments),
//! which is a heuristic and said so in the capability text.
use std::collections::{BTreeMap, HashSet};

use harness_workspace::{
    import_graph::{
        resolve_units, Decision, ImportBuildError, ImportFact, ImportReason, ImportResolver,
        ItemStatus, LanguageProfile, Resolution, ResolveContext, ResolveOutput, SourceUnit,
    },
    repository_intelligence::RootRelativePath,
};

use crate::extract::PACKAGE_ITEM;

pub const RESOLVER_VERSION: u32 = 2;
const PROFILES: [LanguageProfile; 2] = [LanguageProfile::Java, LanguageProfile::Kotlin];
const RULES: [&str; 8] = [
    "java.type_by_package_declaration",
    "java.package_wildcard",
    "java.nested_type",
    "java.stdlib",
    "kotlin.stdlib",
    "java.declared_dependency",
    "java.undeclared_external",
    "java.path_not_found",
];
const STDLIB_PREFIXES: [&str; 6] = ["java.", "javax.", "jdk.", "sun.", "com.sun.", "org.w3c."];
/// The Kotlin standard library, imported by default or explicitly.
const KOTLIN_STDLIB_PREFIXES: [&str; 1] = ["kotlin."];
/// Packages whose artifacts are published under a group id that does not prefix them.
const GROUP_ALIASES: [(&str, &str); 1] = [("kotlinx.", "org.jetbrains.kotlinx")];

#[derive(Clone, Copy, Debug, Default)]
pub struct JavaImportResolver;

impl ImportResolver for JavaImportResolver {
    fn id(&self) -> &'static str {
        "java-resolver"
    }

    fn version(&self) -> u32 {
        RESOLVER_VERSION
    }

    fn profiles(&self) -> Vec<LanguageProfile> {
        PROFILES.to_vec()
    }

    fn rules(&self) -> Vec<&'static str> {
        RULES.to_vec()
    }

    fn unsupported(&self) -> Vec<&'static str> {
        vec![
            "references to types of the same package (no import is written, so no edge is made)",
            "classpath-only resolution, annotation processors and generated sources",
            "matching an import to a Maven or Gradle dependency is by group id prefix; starter or BOM artifacts whose packages differ from their group id may be reported as undeclared",
            "Scala and Groovy sources",
            "Kotlin top-level functions called from Java through their generated `FileNameKt` class",
            "Kotlin default imports (`kotlin.*`, `kotlin.collections.*`, ...) are implicit and make no edge",
        ]
    }

    fn resolve(&self, context: &ResolveContext<'_>) -> Result<ResolveOutput, ImportBuildError> {
        // Fully qualified type name -> file, and package -> files.
        let mut types: BTreeMap<String, Vec<&RootRelativePath>> = BTreeMap::new();
        let mut packages: BTreeMap<String, Vec<&RootRelativePath>> = BTreeMap::new();
        for unit in context
            .units
            .iter()
            .filter(|u| PROFILES.contains(&u.profile))
        {
            let Some(package) = unit
                .extracted
                .items
                .iter()
                .find(|i| i.kind == PACKAGE_ITEM)
                .map(|i| i.name.clone())
            else {
                continue;
            };
            packages
                .entry(package.clone())
                .or_default()
                .push(&unit.path);
            for item in unit
                .extracted
                .items
                .iter()
                .filter(|i| i.kind != PACKAGE_ITEM)
            {
                types
                    .entry(format!("{package}.{}", item.name))
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
        let max = context.limits.max_candidates;
        resolve_units(context, &PROFILES, |unit, fact| {
            decide(unit, fact, &types, &packages, &declared, max)
        })
    }
}

fn found(file: &RootRelativePath, item: Option<String>) -> Resolution {
    Resolution::ResolvedInternal {
        target_file: file.clone(),
        target_item: item,
        item_status: ItemStatus::NotAttempted,
        via: Vec::new(),
        definition_file: None,
    }
}

/// The leading `n` dot-separated segments.
fn lead(name: &str, n: usize) -> String {
    name.split('.').take(n).collect::<Vec<_>>().join(".")
}

fn decide(
    unit: &SourceUnit,
    fact: &ImportFact,
    types: &BTreeMap<String, Vec<&RootRelativePath>>,
    packages: &BTreeMap<String, Vec<&RootRelativePath>>,
    declared: &BTreeMap<&str, HashSet<&str>>,
    max: usize,
) -> Decision {
    let spelling = fact.spelling.as_str();
    let pick = |files: &[&RootRelativePath]| -> Resolution {
        match files {
            [only] => found(only, None),
            many => Resolution::Possible {
                candidates: many.iter().take(max).map(|f| (*f).clone()).collect(),
                truncated: many.len() > max,
            },
        }
    };
    if fact.glob
        && fact.imported_name.as_deref() == Some("*")
        && !fact.scope.iter().any(|s| s == "static")
    {
        // `import a.b.*;` names a package.
        if let Some(files) = packages.get(spelling) {
            return Decision::new(pick(files), "java.package_wildcard");
        }
    }
    if let Some(files) = types.get(spelling) {
        let item = fact.imported_name.clone().filter(|n| n != "*");
        let resolution = match pick(files) {
            Resolution::ResolvedInternal { target_file, .. } => found(&target_file, item),
            other => other,
        };
        return Decision::new(resolution, "java.type_by_package_declaration");
    }
    // `a.b.Outer.Inner`: the longest prefix that is a top-level type.
    let parts: Vec<&str> = spelling.split('.').collect();
    for end in (2..parts.len()).rev() {
        if let Some(files) = types.get(&parts[..end].join(".")) {
            return Decision::new(pick(files), "java.nested_type");
        }
    }
    if KOTLIN_STDLIB_PREFIXES
        .iter()
        .any(|p| spelling.starts_with(p))
    {
        return Decision::new(
            Resolution::DeclaredExternal {
                package: format!("stdlib:{}", lead(spelling, 2)),
            },
            "kotlin.stdlib",
        );
    }
    if STDLIB_PREFIXES.iter().any(|p| spelling.starts_with(p)) {
        return Decision::new(
            Resolution::DeclaredExternal {
                package: format!("stdlib:{}", lead(spelling, 2)),
            },
            "java.stdlib",
        );
    }
    // A package of this repository whose type is missing.
    if packages
        .keys()
        .any(|p| spelling == p || spelling.starts_with(&format!("{p}.")))
    {
        return Decision::new(
            Resolution::Unresolved {
                reason: ImportReason::PathNotFound,
            },
            "java.path_not_found",
        );
    }
    let dependencies = unit
        .package
        .as_ref()
        .and_then(|p| declared.get(p.0.as_str()));
    // `kotlinx.coroutines` is published as `org.jetbrains.kotlinx:kotlinx-coroutines-core`.
    let aliased = GROUP_ALIASES
        .iter()
        .find(|(prefix, _)| spelling.starts_with(prefix))
        .map(|(_, group)| *group);
    let matched = dependencies.and_then(|set| {
        set.iter()
            .filter_map(|coordinate| {
                let group = coordinate.split(':').next()?;
                if aliased == Some(group) {
                    return Some((usize::MAX, *coordinate));
                }
                let shared = if spelling == group || spelling.starts_with(&format!("{group}.")) {
                    group.split('.').count()
                } else if lead(spelling, 2) == lead(group, 2) && group.split('.').count() >= 2 {
                    2
                } else {
                    return None;
                };
                Some((shared, *coordinate))
            })
            .max_by_key(|(shared, coordinate)| (*shared, std::cmp::Reverse(coordinate.to_string())))
    });
    match matched {
        Some((_, coordinate)) => Decision::new(
            Resolution::DeclaredExternal {
                package: coordinate.to_owned(),
            },
            "java.declared_dependency",
        ),
        None => Decision::new(
            Resolution::Unresolved {
                reason: ImportReason::UndeclaredExternal,
            },
            "java.undeclared_external",
        ),
    }
}
