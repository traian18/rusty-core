//! Static resolution of Python imports against the modules found in the repository.
//!
//! Order: relative imports against the importing file's directory; absolute imports against an
//! index of module names built from package directories (`__init__.py` chains), manifest roots and
//! `src/`; then the standard library; then declared distributions (with a table of common
//! import-name aliases). Nothing is executed, no environment is consulted.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use harness_workspace::{
    import_graph::{
        resolve_units, Decision, ImportBuildError, ImportFact, ImportForm, ImportReason,
        ImportResolver, ItemStatus, LanguageProfile, Resolution, ResolveContext, ResolveOutput,
        SourceUnit,
    },
    repository_intelligence::{PackageId, RootRelativePath},
};

use crate::stdlib::{candidate_distributions, is_stdlib, normalize};

pub const RESOLVER_VERSION: u32 = 1;
const RULES: [&str; 8] = [
    "python.relative_import",
    "python.absolute_module",
    "python.submodule_of_package",
    "python.stdlib",
    "python.declared_distribution",
    "python.undeclared_external",
    "python.dynamic_specifier",
    "python.path_not_found",
];

#[derive(Clone, Copy, Debug, Default)]
pub struct PythonImportResolver;

impl ImportResolver for PythonImportResolver {
    fn id(&self) -> &'static str {
        "python-resolver"
    }

    fn version(&self) -> u32 {
        RESOLVER_VERSION
    }

    fn profiles(&self) -> Vec<LanguageProfile> {
        vec![LanguageProfile::Python]
    }

    fn rules(&self) -> Vec<&'static str> {
        RULES.to_vec()
    }

    fn unsupported(&self) -> Vec<&'static str> {
        vec![
            "sys.path edits, .pth files, PYTHONPATH and editable installs",
            "importlib with a computed name, conditional imports chosen at run time",
            "distribution names that differ from import names beyond a small table of common aliases (an unknown one is reported as undeclared)",
            "stub packages (.pyi) and compiled extension modules",
        ]
    }

    fn resolve(&self, context: &ResolveContext<'_>) -> Result<ResolveOutput, ImportBuildError> {
        let index = ModuleIndex::build(context);
        let declared: HashMap<&PackageId, HashSet<String>> = context
            .structure
            .packages
            .iter()
            .map(|p| {
                (
                    &p.id,
                    p.facts
                        .declared_dependencies
                        .iter()
                        .map(|d| normalize(d))
                        .collect(),
                )
            })
            .collect();
        let units: HashMap<&RootRelativePath, &SourceUnit> =
            context.units.iter().map(|u| (&u.path, u)).collect();
        let max = context.limits.max_candidates;
        resolve_units(context, &[LanguageProfile::Python], |unit, fact| {
            decide(unit, fact, &index, &declared, &units, max)
        })
    }
}

/// Dotted module name -> files that define it, from every root a file can be imported from.
struct ModuleIndex {
    modules: BTreeMap<String, Vec<RootRelativePath>>,
    files: HashSet<RootRelativePath>,
}

fn dotted(parts: &[String]) -> String {
    parts.join(".")
}

fn components(path: &Path) -> Vec<String> {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect()
}

impl ModuleIndex {
    fn build(context: &ResolveContext<'_>) -> Self {
        let files: HashSet<RootRelativePath> = context
            .units
            .iter()
            .filter(|u| u.profile == LanguageProfile::Python)
            .map(|u| u.path.clone())
            .collect();
        let package_dirs: HashSet<PathBuf> = files
            .iter()
            .filter(|f| f.as_path().file_name().is_some_and(|n| n == "__init__.py"))
            .map(|f| f.directory().to_path_buf())
            .collect();
        let manifest_roots: Vec<PathBuf> = context
            .structure
            .packages
            .iter()
            .filter(|p| {
                p.manifest.as_path().file_name().is_some_and(|n| {
                    n.to_str().is_some_and(|n| {
                        matches!(n, "pyproject.toml" | "setup.py" | "setup.cfg" | "Pipfile")
                            || n.starts_with("requirements")
                    })
                })
            })
            .flat_map(|p| {
                let dir = p.manifest.directory().to_path_buf();
                [dir.clone(), dir.join("src")]
            })
            .collect();
        let mut modules: BTreeMap<String, Vec<RootRelativePath>> = BTreeMap::new();
        let mut add = |name: String, file: &RootRelativePath| {
            if !name.is_empty() {
                let entry = modules.entry(name).or_default();
                if !entry.contains(file) {
                    entry.push(file.clone());
                }
            }
        };
        for file in &files {
            let path = file.as_path();
            let directory = file.directory().to_path_buf();
            // Root above the highest directory of the `__init__.py` chain that holds this file.
            let mut root = directory.clone();
            while package_dirs.contains(&root) {
                match root.parent() {
                    Some(parent) => root = parent.to_path_buf(),
                    None => break,
                }
            }
            let mut roots = vec![root];
            roots.extend(
                manifest_roots
                    .iter()
                    .filter(|r| path.starts_with(r))
                    .cloned(),
            );
            for root in roots {
                let Ok(relative) = path.strip_prefix(&root) else {
                    continue;
                };
                let mut parts = components(relative);
                let Some(last) = parts.pop() else { continue };
                let stem = last.strip_suffix(".py").unwrap_or(&last).to_owned();
                if stem != "__init__" {
                    parts.push(stem);
                }
                add(dotted(&parts), file);
            }
        }
        for files in modules.values_mut() {
            files.sort();
        }
        Self { modules, files }
    }

    fn lookup(&self, name: &str) -> &[RootRelativePath] {
        self.modules.get(name).map_or(&[], Vec::as_slice)
    }
}

fn pick(
    candidates: &[RootRelativePath],
    unit: &SourceUnit,
    units: &HashMap<&RootRelativePath, &SourceUnit>,
    max: usize,
) -> Option<Resolution> {
    match candidates {
        [] => None,
        [only] => Some(found(only.clone(), None, ItemStatus::NotAttempted)),
        many => {
            let same: Vec<&RootRelativePath> = many
                .iter()
                .filter(|c| {
                    units
                        .get(c)
                        .is_some_and(|u| u.package == unit.package && unit.package.is_some())
                })
                .collect();
            if let [only] = same.as_slice() {
                return Some(found((*only).clone(), None, ItemStatus::NotAttempted));
            }
            let truncated = many.len() > max;
            Some(Resolution::Possible {
                candidates: many.iter().take(max).cloned().collect(),
                truncated,
            })
        }
    }
}

fn found(target: RootRelativePath, item: Option<String>, status: ItemStatus) -> Resolution {
    Resolution::ResolvedInternal {
        target_file: target,
        target_item: item,
        item_status: status,
        via: Vec::new(),
        definition_file: None,
    }
}

/// Whether `file` declares `item` at module level (`Found`), or may provide it through a star import.
fn item_status(
    file: &RootRelativePath,
    item: &str,
    units: &HashMap<&RootRelativePath, &SourceUnit>,
) -> ItemStatus {
    let Some(unit) = units.get(file) else {
        return ItemStatus::NotAttempted;
    };
    if unit.extracted.items.iter().any(|i| i.name == item) {
        ItemStatus::Found
    } else if unit.extracted.facts.iter().any(|f| f.glob) {
        ItemStatus::NotAttempted
    } else {
        ItemStatus::NotFound
    }
}

fn decide(
    unit: &SourceUnit,
    fact: &ImportFact,
    index: &ModuleIndex,
    declared: &HashMap<&PackageId, HashSet<String>>,
    units: &HashMap<&RootRelativePath, &SourceUnit>,
    max: usize,
) -> Decision {
    if fact.form == ImportForm::DynamicImport && !fact.literal_specifier {
        return Decision::new(
            Resolution::DynamicOrUnsupported {
                reason: ImportReason::NonLiteralSpecifier,
            },
            "python.dynamic_specifier",
        );
    }
    let spelling = fact.spelling.as_str();
    let item = fact.imported_name.as_deref().filter(|n| *n != "*");

    if spelling.starts_with('.') {
        return relative(unit, fact, spelling, item, index, units, max);
    }

    // `from pkg import name`: the submodule `pkg.name` wins over an attribute of `pkg`.
    if let Some(name) = item {
        let submodule = format!("{spelling}.{name}");
        if let Some(resolution) = pick(index.lookup(&submodule), unit, units, max) {
            return Decision::new(resolution, "python.submodule_of_package");
        }
    }
    if let Some(resolution) = pick(index.lookup(spelling), unit, units, max) {
        return Decision::new(with_item(resolution, item, units), "python.absolute_module");
    }
    let parts: Vec<&str> = spelling.split('.').collect();
    let top = parts[0];
    if is_stdlib(top) {
        return Decision::new(
            Resolution::DeclaredExternal {
                package: format!("stdlib:{top}"),
            },
            "python.stdlib",
        );
    }
    // A top-level name that exists in the repository but whose submodule is missing.
    if !index.lookup(top).is_empty() {
        return Decision::new(
            Resolution::Unresolved {
                reason: ImportReason::PathNotFound,
            },
            "python.path_not_found",
        );
    }
    let dependencies = unit.package.as_ref().and_then(|p| declared.get(p));
    if let Some(distribution) = dependencies.and_then(|set| {
        candidate_distributions(top)
            .into_iter()
            .find(|c| set.contains(c))
    }) {
        return Decision::new(
            Resolution::DeclaredExternal {
                package: distribution,
            },
            "python.declared_distribution",
        );
    }
    Decision::new(
        Resolution::Unresolved {
            reason: ImportReason::UndeclaredExternal,
        },
        "python.undeclared_external",
    )
}

fn with_item(
    resolution: Resolution,
    item: Option<&str>,
    units: &HashMap<&RootRelativePath, &SourceUnit>,
) -> Resolution {
    match (resolution, item) {
        (
            Resolution::ResolvedInternal {
                target_file,
                via,
                definition_file,
                ..
            },
            Some(name),
        ) => {
            let status = item_status(&target_file, name, units);
            Resolution::ResolvedInternal {
                target_file,
                target_item: Some(name.to_owned()),
                item_status: status,
                via,
                definition_file,
            }
        }
        (other, _) => other,
    }
}

fn relative(
    unit: &SourceUnit,
    fact: &ImportFact,
    spelling: &str,
    item: Option<&str>,
    index: &ModuleIndex,
    units: &HashMap<&RootRelativePath, &SourceUnit>,
    max: usize,
) -> Decision {
    let dots = spelling.chars().take_while(|c| *c == '.').count();
    let rest = &spelling[dots..];
    let mut base = unit.path.directory().to_path_buf();
    for _ in 1..dots {
        match base.parent() {
            Some(parent) => base = parent.to_path_buf(),
            None => {
                return Decision::new(
                    Resolution::Unresolved {
                        reason: ImportReason::PathNotFound,
                    },
                    "python.path_not_found",
                )
            }
        }
    }
    let module_file = |parts: &[&str]| -> Option<RootRelativePath> {
        let mut path = base.clone();
        for part in parts {
            path.push(part);
        }
        let as_file = RootRelativePath::new(path.with_extension("py"))
            .ok()
            .filter(|p| index.files.contains(p));
        let as_package = RootRelativePath::new(path.join("__init__.py"))
            .ok()
            .filter(|p| index.files.contains(p));
        as_file.or(as_package)
    };
    let parts: Vec<&str> = rest.split('.').filter(|p| !p.is_empty()).collect();
    if let Some(name) = item {
        let mut with_name = parts.clone();
        with_name.push(name);
        if let Some(target) = module_file(&with_name) {
            return Decision::new(
                found(target, None, ItemStatus::NotAttempted),
                "python.relative_import",
            );
        }
    }
    match module_file(&parts) {
        Some(target) => {
            let status = item.map_or(ItemStatus::NotAttempted, |n| item_status(&target, n, units));
            Decision::new(
                found(target, item.map(str::to_owned), status),
                "python.relative_import",
            )
        }
        None => {
            let _ = (fact, max);
            Decision::new(
                Resolution::Unresolved {
                    reason: ImportReason::PathNotFound,
                },
                "python.path_not_found",
            )
        }
    }
}
