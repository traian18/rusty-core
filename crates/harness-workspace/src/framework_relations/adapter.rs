//! The contract framework adapters implement, and the analyzer that runs them.
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

use tokio_util::sync::CancellationToken;

use super::model::{
    FrameworkApplication, FrameworkCapability, FrameworkIndex, FrameworkIndexParts,
    FrameworkLimits, FrameworkRelation,
};
use crate::{
    import_graph::{ImportIndex, IndexedFile},
    repository_intelligence::{
        read_contained_directory, read_contained_file, DiscoveryDiagnostic, DiscoveryError,
        Package, RepositoryStructure, RootRelativePath,
    },
};

/// Maps a name bound by an import (`import Foo from './x'`, `use crate::a::b`) to the file the
/// import resolved to, so an adapter can follow a name to its definition without re-resolving imports.
#[derive(Default)]
pub struct NameResolver {
    targets: HashMap<(RootRelativePath, String), RootRelativePath>,
}

impl NameResolver {
    pub fn from_imports(imports: &ImportIndex) -> Self {
        let mut targets = HashMap::new();
        for edge in &imports.edges {
            let Some(target) = edge.resolution.target_file() else {
                continue;
            };
            let fact = &edge.fact;
            let bound = match (&fact.alias, &fact.imported_name) {
                (Some(alias), _) => alias.clone(),
                (None, Some(name)) if name != "default" && name != "*" => name.clone(),
                _ => continue,
            };
            targets
                .entry((fact.importing_file.clone(), bound))
                .or_insert_with(|| target.clone());
        }
        Self { targets }
    }

    /// The file a name used in `file` was imported from, if the import resolved to an indexed file.
    pub fn target(&self, file: &RootRelativePath, name: &str) -> Option<&RootRelativePath> {
        self.targets.get(&(file.clone(), name.to_owned()))
    }
}

/// Read access an adapter has to the repository: the structure, the import index, and bounded,
/// contained file reads. Nothing here runs a process or touches the network.
pub struct FrameworkInput<'a> {
    pub root: &'a Path,
    pub structure: &'a RepositoryStructure,
    pub imports: &'a ImportIndex,
    pub names: &'a NameResolver,
    pub limits: &'a FrameworkLimits,
}

impl FrameworkInput<'_> {
    /// The indexed source files of one package, in path order, up to the per-package limit.
    pub fn files_of(&self, package: &Package) -> Vec<&IndexedFile> {
        self.imports
            .files
            .iter()
            .filter(|file| file.package.as_ref() == Some(&package.id))
            .take(self.limits.max_files_per_package)
            .collect()
    }

    /// The directory a package lives in, relative to the root (empty for the root itself).
    pub fn package_dir(package: &Package) -> PathBuf {
        package
            .manifest
            .as_path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    /// A file's text, or `None` when it is unreadable, too large, or not UTF-8.
    pub fn read(&self, path: &RootRelativePath) -> Option<String> {
        let bytes =
            read_contained_file(self.root, path.as_path(), self.limits.max_file_bytes).ok()?;
        String::from_utf8(bytes).ok()
    }

    /// Files with one extension under `directory`, sorted, skipping dependency and build
    /// directories, up to the per-package limit. For formats the import index does not cover
    /// (single-file components, templates).
    pub fn list_files(&self, directory: &Path, extension: &str) -> Vec<RootRelativePath> {
        const SKIPPED: [&str; 6] = [".git", "node_modules", "target", "dist", "build", ".next"];
        let mut found = Vec::new();
        let mut pending = vec![directory.to_path_buf()];
        while let Some(current) = pending.pop() {
            let Ok(entries) = read_contained_directory(self.root, &current) else {
                continue;
            };
            for entry in entries {
                let name = entry.name.to_string_lossy().into_owned();
                let path = current.join(&entry.name);
                if entry.is_directory {
                    if !SKIPPED.contains(&name.as_str()) {
                        pending.push(path);
                    }
                } else if entry.is_regular_file
                    && path.extension().and_then(|e| e.to_str()) == Some(extension)
                {
                    if let Ok(relative) = RootRelativePath::new(&path) {
                        found.push(relative);
                    }
                }
            }
            if found.len() >= self.limits.max_files_per_package {
                break;
            }
        }
        found.sort();
        found.truncate(self.limits.max_files_per_package);
        found
    }
}

/// What an adapter returns for one package.
#[derive(Default)]
pub struct FrameworkOutput {
    pub relations: Vec<FrameworkRelation>,
    /// Conventions seen but not readable statically; each is reported, none becomes a relation.
    pub diagnostics: Vec<DiscoveryDiagnostic>,
}

impl FrameworkOutput {
    pub fn relate(&mut self, relation: FrameworkRelation) {
        self.relations.push(relation);
    }

    pub fn unresolved(&mut self, file: &RootRelativePath, message: impl Into<String>) {
        self.diagnostics
            .push(DiscoveryDiagnostic::for_path(file, message));
    }
}

/// Reads one framework's conventions. Implementations live outside this crate and are registered
/// through `RepositoryGraphBuilder::framework_adapter`.
pub trait FrameworkAdapter: Send + Sync {
    /// Short stable id, for example `express`.
    fn id(&self) -> &'static str;
    fn version(&self) -> u32;
    fn capability(&self) -> FrameworkCapability;
    /// Why the framework applies to this package (a declared dependency, a configuration file),
    /// or `None` when it does not or applicability is unknown. With `None` the adapter is off.
    fn applicability(&self, package: &Package) -> Option<String>;
    fn extract(
        &self,
        package: &Package,
        input: &FrameworkInput<'_>,
        cancel: &CancellationToken,
        output: &mut FrameworkOutput,
    ) -> Result<(), DiscoveryError>;
}

/// `Some("dependency `name`")` when the package declares `name`, for use in `applicability`.
pub fn declares(package: &Package, name: &str) -> Option<String> {
    package
        .facts
        .declared_dependencies
        .iter()
        .any(|dependency| dependency == name)
        .then(|| format!("declares dependency `{name}`"))
}

/// Runs a set of adapters over a finished structure and import index.
pub trait FrameworkAnalyzer: Send + Sync {
    fn limits(&self) -> FrameworkLimits;
    fn analyze(
        &self,
        root: &Path,
        structure: &RepositoryStructure,
        imports: &ImportIndex,
        cancel: &CancellationToken,
    ) -> Result<FrameworkIndex, DiscoveryError>;
}

/// Applies each registered adapter to every package it applies to, enforces the limits, and
/// builds the index. A framework that does not apply to a package contributes nothing to it.
pub struct DefaultFrameworkAnalyzer {
    adapters: Vec<std::sync::Arc<dyn FrameworkAdapter>>,
    limits: FrameworkLimits,
}

impl DefaultFrameworkAnalyzer {
    pub fn new(
        adapters: Vec<std::sync::Arc<dyn FrameworkAdapter>>,
        limits: FrameworkLimits,
    ) -> Self {
        Self { adapters, limits }
    }
}

impl FrameworkAnalyzer for DefaultFrameworkAnalyzer {
    fn limits(&self) -> FrameworkLimits {
        self.limits.clone()
    }

    fn analyze(
        &self,
        root: &Path,
        structure: &RepositoryStructure,
        imports: &ImportIndex,
        cancel: &CancellationToken,
    ) -> Result<FrameworkIndex, DiscoveryError> {
        let names = NameResolver::from_imports(imports);
        let input = FrameworkInput {
            root,
            structure,
            imports,
            names: &names,
            limits: &self.limits,
        };
        let mut packages = structure.packages.iter().collect::<Vec<_>>();
        packages.sort_by(|a, b| a.id.cmp(&b.id));

        let mut relations: Vec<FrameworkRelation> = Vec::new();
        let mut diagnostics = Vec::new();
        let mut applications = Vec::new();
        let mut partial = false;
        for package in packages {
            for adapter in &self.adapters {
                if cancel.is_cancelled() {
                    return Err(DiscoveryError::Cancelled);
                }
                let Some(evidence) = adapter.applicability(package) else {
                    continue;
                };
                applications.push(FrameworkApplication {
                    framework: adapter.id().to_owned(),
                    package: package.id.clone(),
                    evidence,
                });
                let mut output = FrameworkOutput::default();
                adapter.extract(package, &input, cancel, &mut output)?;
                diagnostics.extend(output.diagnostics);

                // Bound relations per file, keeping the earliest in the file.
                let mut per_file: BTreeMap<RootRelativePath, usize> = BTreeMap::new();
                output.relations.sort_by(|a, b| {
                    (&a.from, a.evidence.span.start).cmp(&(&b.from, b.evidence.span.start))
                });
                for relation in output.relations {
                    let count = per_file.entry(relation.from.clone()).or_default();
                    if *count >= self.limits.max_relations_per_file {
                        if *count == self.limits.max_relations_per_file {
                            diagnostics.push(DiscoveryDiagnostic::for_path(
                                &relation.from,
                                format!(
                                    "{} relations in this file exceed the limit of {}; the rest were left out",
                                    adapter.id(),
                                    self.limits.max_relations_per_file
                                ),
                            ));
                            *count += 1;
                        }
                        partial = true;
                        continue;
                    }
                    *count += 1;
                    relations.push(relation);
                }
            }
        }
        if relations.len() > self.limits.max_relations {
            relations.truncate(self.limits.max_relations);
            partial = true;
            diagnostics.push(DiscoveryDiagnostic {
                path: None,
                message: format!(
                    "framework relations exceed the limit of {}; the rest were left out",
                    self.limits.max_relations
                ),
            });
        }
        Ok(FrameworkIndex::new(FrameworkIndexParts {
            relations,
            diagnostics,
            applications,
            capabilities: self
                .adapters
                .iter()
                .map(|adapter| adapter.capability())
                .collect(),
            partial,
            limits: self.limits.clone(),
        }))
    }
}
