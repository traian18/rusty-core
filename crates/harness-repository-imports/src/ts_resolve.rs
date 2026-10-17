//! Static resolution of TypeScript/JavaScript specifiers.
//!
//! Order for a bare specifier: Node builtin, tsconfig `paths`, tsconfig `baseUrl`,
//! npm workspace package, declared dependency. Relative specifiers use TypeScript's
//! extension and `index` probing. Nothing is executed and no package is installed.
use std::collections::HashMap;

use harness_workspace::{
    import_graph::{
        ImportBuildError, ImportFact, ImportForm, ImportReason, ImportResolver, ItemStatus,
        LanguageProfile, ReexportHop, Resolution, ResolveContext, ResolveOutput, ResolvedImport,
        SourceUnit,
    },
    repository_intelligence::{Package, RootRelativePath},
};

use crate::{
    paths::{file_name, parent},
    ts_config::{CompilerOptions, TsConfigIndex},
    ts_files::{is_asset, split_package, ScriptFiles},
};

pub const TS_RESOLVER_VERSION: u32 = 2;
const STEP_BUDGET: usize = 2_000;
const NODE_BUILTINS: [&str; 24] = [
    "fs",
    "path",
    "os",
    "http",
    "https",
    "crypto",
    "url",
    "util",
    "stream",
    "events",
    "child_process",
    "assert",
    "buffer",
    "zlib",
    "net",
    "tls",
    "dns",
    "readline",
    "worker_threads",
    "cluster",
    "module",
    "process",
    "perf_hooks",
    "querystring",
];
const RULES: [&str; 11] = [
    "ts.relative_path",
    "ts.tsconfig_paths",
    "ts.tsconfig_base_url",
    "ts.workspace_package_entry",
    "ts.workspace_package_subpath",
    "ts.declared_external",
    "ts.node_builtin",
    "ts.dynamic_specifier",
    "ts.alias_unsupported",
    "ts.asset_import",
    "ts.no_match",
];

#[derive(Clone, Copy, Debug, Default)]
pub struct TypeScriptImportResolver;

impl ImportResolver for TypeScriptImportResolver {
    fn id(&self) -> &'static str {
        "typescript-javascript-resolver"
    }

    fn version(&self) -> u32 {
        TS_RESOLVER_VERSION
    }

    fn profiles(&self) -> Vec<LanguageProfile> {
        vec![LanguageProfile::TypeScript, LanguageProfile::JavaScript]
    }

    fn rules(&self) -> Vec<&'static str> {
        RULES.to_vec()
    }

    fn unsupported(&self) -> Vec<&'static str> {
        vec![
            "tsconfig extends from npm packages, include/exclude/references, rootDirs, moduleSuffixes",
            "bundler aliases and conditional exports other than types, import, default, require",
            "package.json imports (# specifiers)",
            "imports of non-script files (json, css, images)",
            "absolute-path specifiers",
            "require() and import() with non-literal arguments",
        ]
    }

    fn resolve(&self, context: &ResolveContext<'_>) -> Result<ResolveOutput, ImportBuildError> {
        let units = context
            .units
            .iter()
            .filter(|unit| {
                matches!(
                    unit.profile,
                    LanguageProfile::TypeScript | LanguageProfile::JavaScript
                )
            })
            .collect::<Vec<_>>();
        let configs = TsConfigIndex::build(context.configs);
        let mut model = Model::new(context, &units, &configs);
        let mut output = ResolveOutput {
            diagnostics: configs.diagnostics.clone(),
            ..ResolveOutput::default()
        };
        for unit in &units {
            if context.cancel.is_cancelled() {
                return Err(ImportBuildError::Cancelled);
            }
            for fact in &unit.extracted.facts {
                let resolved = model.resolve_fact(unit, fact);
                if let Some(message) = model.exhausted.take() {
                    output.diagnostics.push(
                        harness_workspace::repository_intelligence::DiscoveryDiagnostic {
                            path: Some(unit.path.clone()),
                            message,
                        },
                    );
                }
                output.resolved.push(resolved);
            }
        }
        Ok(output)
    }
}

/// The module a specifier names, before any item lookup.
enum Target {
    File {
        file: RootRelativePath,
        rule: &'static str,
        evidence: Vec<RootRelativePath>,
    },
    Other {
        resolution: Resolution,
        rule: &'static str,
        evidence: Vec<RootRelativePath>,
    },
}

impl Target {
    fn file(file: RootRelativePath, rule: &'static str) -> Self {
        Self::File {
            file,
            rule,
            evidence: Vec::new(),
        }
    }

    fn other(resolution: Resolution, rule: &'static str) -> Self {
        Self::Other {
            resolution,
            rule,
            evidence: Vec::new(),
        }
    }

    fn unresolved(reason: ImportReason, rule: &'static str) -> Self {
        Self::other(Resolution::Unresolved { reason }, rule)
    }

    fn with_evidence(mut self, more: Vec<RootRelativePath>) -> Self {
        match &mut self {
            Self::File { evidence, .. } | Self::Other { evidence, .. } => *evidence = more,
        }
        self
    }
}

/// How the specifier is spelled, which decides the resolution path.
enum Spelling {
    Relative,
    Absolute,
    NodeBuiltin,
    PackageImport,
    Bare,
}

fn classify(specifier: &str) -> Spelling {
    if specifier == "."
        || specifier == ".."
        || specifier.starts_with("./")
        || specifier.starts_with("../")
    {
        Spelling::Relative
    } else if specifier.starts_with('/') {
        Spelling::Absolute
    } else if specifier.starts_with("node:")
        || NODE_BUILTINS.contains(&specifier.split('/').next().unwrap_or(""))
    {
        Spelling::NodeBuiltin
    } else if specifier.starts_with('#') {
        Spelling::PackageImport
    } else {
        Spelling::Bare
    }
}

struct Model<'a> {
    context: &'a ResolveContext<'a>,
    units: HashMap<&'a RootRelativePath, &'a SourceUnit>,
    scripts: ScriptFiles<'a>,
    npm: HashMap<String, Vec<&'a Package>>,
    configs: &'a TsConfigIndex,
    exhausted: Option<String>,
}

impl<'a> Model<'a> {
    fn new(
        context: &'a ResolveContext<'a>,
        units: &[&'a SourceUnit],
        configs: &'a TsConfigIndex,
    ) -> Self {
        let mut npm: HashMap<String, Vec<&Package>> = HashMap::new();
        for package in context
            .structure
            .packages
            .iter()
            .filter(|p| file_name(&p.manifest) == "package.json")
        {
            npm.entry(package.name.clone()).or_default().push(package);
        }
        Self {
            context,
            units: units.iter().map(|u| (&u.path, *u)).collect(),
            scripts: ScriptFiles::new(units.iter().map(|u| &u.path)),
            npm,
            configs,
            exhausted: None,
        }
    }

    fn resolve_fact(&mut self, unit: &SourceUnit, fact: &ImportFact) -> ResolvedImport {
        let mut steps = 0;
        let (resolution, rule, evidence) = match self.resolve_target(unit, fact) {
            Target::Other {
                resolution,
                rule,
                evidence,
            } => (resolution, rule, evidence),
            Target::File {
                file,
                rule,
                evidence,
            } => (self.resolve_item(file, fact, &mut steps), rule, evidence),
        };
        ResolvedImport {
            fact: fact.id.clone(),
            resolution,
            rule: rule.to_owned(),
            evidence,
        }
    }

    /// Attaches the imported name to the target file, following re-exports.
    fn resolve_item(
        &mut self,
        file: RootRelativePath,
        fact: &ImportFact,
        steps: &mut usize,
    ) -> Resolution {
        let name = match (fact.imported_name.as_deref(), fact.form) {
            (Some(name), ImportForm::Import | ImportForm::ExportFrom) if name != "*" => {
                name.to_owned()
            }
            _ => return internal(file, None, ItemStatus::NotAttempted, Vec::new(), None),
        };
        match self.find_item(&file, &name, 0, steps) {
            Some((via, definition)) => {
                internal(file, Some(name), ItemStatus::Found, via, definition)
            }
            None => internal(file, Some(name), ItemStatus::NotFound, Vec::new(), None),
        }
    }

    fn resolve_target(&self, unit: &SourceUnit, fact: &ImportFact) -> Target {
        if !fact.literal_specifier {
            return Target::other(
                Resolution::DynamicOrUnsupported {
                    reason: ImportReason::NonLiteralSpecifier,
                },
                "ts.dynamic_specifier",
            );
        }
        let specifier = fact.spelling.as_str();
        match classify(specifier) {
            Spelling::Relative => self.resolve_relative(unit, specifier),
            Spelling::Absolute => Target::other(
                Resolution::DynamicOrUnsupported {
                    reason: ImportReason::UnsupportedForm,
                },
                "ts.no_match",
            ),
            Spelling::NodeBuiltin => Target::other(
                Resolution::DeclaredExternal {
                    package: specifier.to_owned(),
                },
                "ts.node_builtin",
            ),
            Spelling::PackageImport => {
                Target::unresolved(ImportReason::AliasUnsupported, "ts.alias_unsupported")
            }
            Spelling::Bare => self.resolve_bare(unit, specifier),
        }
    }

    fn resolve_relative(&self, unit: &SourceUnit, specifier: &str) -> Target {
        if is_asset(specifier) {
            return Target::other(
                Resolution::DynamicOrUnsupported {
                    reason: ImportReason::UnsupportedForm,
                },
                "ts.asset_import",
            );
        }
        match self.scripts.probe(parent(&unit.path), specifier) {
            Some(file) => Target::file(file, "ts.relative_path"),
            None => Target::unresolved(ImportReason::PathNotFound, "ts.no_match"),
        }
    }

    fn resolve_bare(&self, unit: &SourceUnit, specifier: &str) -> Target {
        let options = self.configs.options_for(&unit.path);
        if let Some(options) = options {
            if let Some(target) = self.resolve_via_tsconfig(options, specifier) {
                return target;
            }
        }
        let (name, subpath) = split_package(specifier);
        if let Some(packages) = self.npm.get(name) {
            return self.workspace_package(packages, subpath);
        }
        if self.is_declared(unit, name) {
            return Target::other(
                Resolution::DeclaredExternal {
                    package: name.to_owned(),
                },
                "ts.declared_external",
            );
        }
        let matched_paths = options.is_some_and(|o| o.path_candidates(specifier).is_some());
        if matched_paths {
            let chain = options.map(|o| o.chain.clone()).unwrap_or_default();
            return Target::unresolved(ImportReason::PathNotFound, "ts.tsconfig_paths")
                .with_evidence(chain);
        }
        if specifier.starts_with("@/") || specifier.starts_with("~/") {
            return Target::unresolved(ImportReason::AliasUnsupported, "ts.alias_unsupported");
        }
        Target::unresolved(ImportReason::UndeclaredExternal, "ts.no_match")
    }

    /// `paths` first, then `baseUrl`, like the TypeScript compiler.
    fn resolve_via_tsconfig(&self, options: &CompilerOptions, specifier: &str) -> Option<Target> {
        if let Some(file) = options
            .path_candidates(specifier)
            .into_iter()
            .flatten()
            .find_map(|candidate| self.scripts.probe_path(candidate.as_path()))
        {
            return Some(
                Target::file(file, "ts.tsconfig_paths").with_evidence(options.chain.clone()),
            );
        }
        let file = self
            .scripts
            .probe_path(options.base_url_candidate(specifier)?.as_path())?;
        Some(Target::file(file, "ts.tsconfig_base_url").with_evidence(options.chain.clone()))
    }

    /// A dependency counts as declared by its own name or its `@types/` stub.
    fn is_declared(&self, unit: &SourceUnit, name: &str) -> bool {
        let types_name = format!("@types/{}", name.trim_start_matches('@').replace('/', "__"));
        unit.package
            .as_ref()
            .and_then(|id| self.context.structure.packages.iter().find(|p| &p.id == id))
            .is_some_and(|package| {
                package
                    .facts
                    .declared_dependencies
                    .iter()
                    .any(|dep| dep == name || *dep == types_name)
            })
    }

    fn workspace_package(&self, packages: &[&Package], subpath: Option<&str>) -> Target {
        let rule = if subpath.is_some() {
            "ts.workspace_package_subpath"
        } else {
            "ts.workspace_package_entry"
        };
        let found = packages
            .iter()
            .filter_map(|package| {
                self.package_target(package, subpath)
                    .map(|file| (file, package.manifest.clone()))
            })
            .collect::<Vec<_>>();
        match found.as_slice() {
            [] => Target::unresolved(
                if subpath.is_some() {
                    ImportReason::SubpathUnsupported
                } else {
                    ImportReason::PathNotFound
                },
                rule,
            ),
            [(file, manifest)] => {
                Target::file(file.clone(), rule).with_evidence(vec![manifest.clone()])
            }
            many => {
                let limit = self.context.limits.max_candidates;
                Target::other(
                    Resolution::Possible {
                        truncated: many.len() > limit,
                        candidates: many.iter().take(limit).map(|(f, _)| f.clone()).collect(),
                    },
                    rule,
                )
            }
        }
    }

    /// The file a workspace package exposes: its declared entry, `index`, or `src/index`.
    fn package_target(&self, package: &Package, subpath: Option<&str>) -> Option<RootRelativePath> {
        let base = parent(&package.manifest);
        match subpath {
            Some(sub) => self
                .scripts
                .probe(base, sub)
                .or_else(|| self.scripts.probe(&base.join("src"), sub)),
            None => package
                .facts
                .entry_points
                .iter()
                .find_map(|entry| self.scripts.probe_path(entry.as_path()))
                .or_else(|| self.scripts.probe(base, "index"))
                .or_else(|| self.scripts.probe(&base.join("src"), "index")),
        }
    }

    /// Looks for an export named `name` in `file`, following `export ... from` chains.
    fn find_item(
        &mut self,
        file: &RootRelativePath,
        name: &str,
        hops: usize,
        steps: &mut usize,
    ) -> Option<(Vec<ReexportHop>, Option<RootRelativePath>)> {
        let unit = self.units.get(file).copied()?;
        if unit.extracted.items.iter().any(|item| item.name == name) {
            return Some((Vec::new(), None));
        }
        if hops >= self.context.limits.max_reexport_hops || *steps >= STEP_BUDGET {
            self.exhausted = Some(format!(
                "re-export chain exceeded the configured limit while resolving `{name}`"
            ));
            return None;
        }
        *steps += 1;
        let reexports = unit
            .extracted
            .facts
            .iter()
            .filter(|fact| {
                fact.reexport
                    && (fact.glob
                        || fact.alias.as_deref().or(fact.imported_name.as_deref()) == Some(name))
            })
            .collect::<Vec<_>>();
        for fact in reexports {
            let Target::File { file: next, .. } = self.resolve_target(unit, fact) else {
                continue;
            };
            let hop = ReexportHop {
                file: file.clone(),
                fact: fact.id.clone(),
            };
            // `export { a as b } from` looks up `a`; `export *` looks up the same name.
            let wanted = if fact.glob || fact.imported_name.as_deref() == Some("*") {
                name.to_owned()
            } else {
                fact.imported_name
                    .clone()
                    .unwrap_or_else(|| name.to_owned())
            };
            if let Some((mut via, definition)) = self.find_item(&next, &wanted, hops + 1, steps) {
                via.insert(0, hop);
                return Some((via, definition.or(Some(next))));
            }
        }
        None
    }
}

fn internal(
    target_file: RootRelativePath,
    target_item: Option<String>,
    item_status: ItemStatus,
    via: Vec<ReexportHop>,
    definition_file: Option<RootRelativePath>,
) -> Resolution {
    Resolution::ResolvedInternal {
        target_file,
        target_item,
        item_status,
        via,
        definition_file,
    }
}
