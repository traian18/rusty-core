//! Static resolution of PHP `use` and literal `require`/`include`.
//!
//! A `use` names a class, interface, trait or enum by namespace; it resolves to the file whose
//! `namespace` and declaration match (PSR-4 maps are recorded as facts but resolution does not
//! depend on them). Other names are matched to Composer packages by vendor. Literal `require`
//! paths resolve against the including file's directory.
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use harness_workspace::{
    import_graph::{
        resolve_units, Decision, ImportBuildError, ImportFact, ImportForm, ImportReason,
        ImportResolver, ItemStatus, LanguageProfile, Resolution, ResolveContext, ResolveOutput,
        SourceUnit,
    },
    repository_intelligence::RootRelativePath,
};

use crate::extract::NAMESPACE_ITEM;

pub const RESOLVER_VERSION: u32 = 1;
const RULES: [&str; 8] = [
    "php.class_by_namespace_declaration",
    "php.namespace_declaration",
    "php.require_literal",
    "php.composer_autoload",
    "php.builtin",
    "php.declared_package",
    "php.undeclared_external",
    "php.path_not_found",
];
/// Namespaces whose Composer package is not named after the namespace's first segment.
const NAMESPACE_PACKAGES: [(&str, &str); 8] = [
    ("Illuminate", "laravel/framework"),
    ("Carbon", "nesbot/carbon"),
    ("Faker", "fakerphp/faker"),
    ("Psr", "psr/"),
    ("PHPUnit", "phpunit/phpunit"),
    ("Doctrine", "doctrine/"),
    ("Symfony", "symfony/"),
    ("Twig", "twig/twig"),
];

#[derive(Clone, Copy, Debug, Default)]
pub struct PhpImportResolver;

impl ImportResolver for PhpImportResolver {
    fn id(&self) -> &'static str {
        "php-resolver"
    }

    fn version(&self) -> u32 {
        RESOLVER_VERSION
    }

    fn profiles(&self) -> Vec<LanguageProfile> {
        vec![LanguageProfile::Php]
    }

    fn rules(&self) -> Vec<&'static str> {
        RULES.to_vec()
    }

    fn unsupported(&self) -> Vec<&'static str> {
        vec![
            "autoloaders registered at run time, class_alias and classes created dynamically",
            "require/include with computed paths (reported, not followed) and include_path lookups",
            "references to classes of the same namespace (no use is written, so no edge is made)",
            "matching a namespace to a Composer package is by vendor name and a short table; others may be reported as undeclared",
        ]
    }

    fn resolve(&self, context: &ResolveContext<'_>) -> Result<ResolveOutput, ImportBuildError> {
        let mut classes: BTreeMap<String, Vec<&RootRelativePath>> = BTreeMap::new();
        let mut namespaces: BTreeMap<String, Vec<&RootRelativePath>> = BTreeMap::new();
        for unit in context
            .units
            .iter()
            .filter(|u| u.profile == LanguageProfile::Php)
        {
            for item in &unit.extracted.items {
                if item.kind == NAMESPACE_ITEM {
                    namespaces
                        .entry(item.name.clone())
                        .or_default()
                        .push(&unit.path);
                } else {
                    let namespace = item.scope.first().cloned().unwrap_or_default();
                    let full = if namespace.is_empty() {
                        item.name.clone()
                    } else {
                        format!("{namespace}\\{}", item.name)
                    };
                    classes.entry(full).or_default().push(&unit.path);
                }
            }
        }
        let files: HashSet<&RootRelativePath> = context
            .units
            .iter()
            .filter(|u| u.profile == LanguageProfile::Php)
            .map(|u| &u.path)
            .collect();
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
        resolve_units(context, &[LanguageProfile::Php], |unit, fact| {
            decide(unit, fact, &classes, &namespaces, &files, &declared, max)
        })
    }
}

fn found(files: &[&RootRelativePath], max: usize) -> Resolution {
    match files {
        [only] => Resolution::ResolvedInternal {
            target_file: (*only).clone(),
            target_item: None,
            item_status: ItemStatus::NotAttempted,
            via: Vec::new(),
            definition_file: None,
        },
        many => Resolution::Possible {
            candidates: many.iter().take(max).map(|f| (*f).clone()).collect(),
            truncated: many.len() > max,
        },
    }
}

fn normalize(path: &Path) -> Option<std::path::PathBuf> {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(p) => parts.push(p.to_owned()),
            std::path::Component::ParentDir => {
                parts.pop()?;
            }
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.iter().collect())
}

fn decide(
    unit: &SourceUnit,
    fact: &ImportFact,
    classes: &BTreeMap<String, Vec<&RootRelativePath>>,
    namespaces: &BTreeMap<String, Vec<&RootRelativePath>>,
    files: &HashSet<&RootRelativePath>,
    declared: &BTreeMap<&str, HashSet<&str>>,
    max: usize,
) -> Decision {
    if fact.form == ImportForm::Require {
        if !fact.literal_specifier {
            return Decision::new(
                Resolution::DynamicOrUnsupported {
                    reason: ImportReason::NonLiteralSpecifier,
                },
                "php.require_literal",
            );
        }
        if fact.spelling.ends_with("vendor/autoload.php") {
            return Decision::new(
                Resolution::DeclaredExternal {
                    package: "composer-autoload".into(),
                },
                "php.composer_autoload",
            );
        }
        let joined = unit
            .path
            .as_path()
            .parent()
            .unwrap_or(Path::new(""))
            .join(fact.spelling.trim_start_matches('/'));
        return match normalize(&joined)
            .and_then(|p| RootRelativePath::new(p).ok())
            .filter(|p| files.contains(p))
        {
            Some(target) => Decision::new(found(&[&target], max), "php.require_literal"),
            None => Decision::new(
                Resolution::Unresolved {
                    reason: ImportReason::PathNotFound,
                },
                "php.path_not_found",
            ),
        };
    }
    let name = fact.spelling.as_str();
    if let Some(files) = classes.get(name) {
        return Decision::new(found(files, max), "php.class_by_namespace_declaration");
    }
    if let Some(files) = namespaces.get(name) {
        return Decision::new(found(files, max), "php.namespace_declaration");
    }
    if !name.contains('\\') {
        return Decision::new(
            Resolution::DeclaredExternal {
                package: "php-builtin".into(),
            },
            "php.builtin",
        );
    }
    let first = name.split('\\').next().unwrap_or("");
    // A namespace of this repository whose class is missing.
    if namespaces
        .keys()
        .any(|n| n.split('\\').next() == Some(first))
    {
        return Decision::new(
            Resolution::Unresolved {
                reason: ImportReason::PathNotFound,
            },
            "php.path_not_found",
        );
    }
    let dependencies = unit
        .package
        .as_ref()
        .and_then(|p| declared.get(p.0.as_str()));
    let lowered = first.to_lowercase();
    let matched = dependencies.and_then(|set| {
        let mut candidates: Vec<&str> = set
            .iter()
            .copied()
            .filter(|pkg| {
                let vendor = pkg.split('/').next().unwrap_or("");
                vendor == lowered
                    || NAMESPACE_PACKAGES.iter().any(|(ns, want)| {
                        *ns == first
                            && (pkg == want || (want.ends_with('/') && pkg.starts_with(want)))
                    })
            })
            .collect();
        candidates.sort();
        // Prefer a package whose own name also appears in the namespace (monolog/monolog for Monolog\…).
        let second = name
            .split('\\')
            .nth(1)
            .map(str::to_lowercase)
            .unwrap_or_default();
        candidates
            .iter()
            .find(|p| p.split('/').nth(1) == Some(second.as_str()))
            .or_else(|| candidates.first())
            .copied()
    });
    match matched {
        Some(package) => Decision::new(
            Resolution::DeclaredExternal {
                package: package.to_owned(),
            },
            "php.declared_package",
        ),
        None => Decision::new(
            Resolution::Unresolved {
                reason: ImportReason::UndeclaredExternal,
            },
            "php.undeclared_external",
        ),
    }
}
