//! Filesystem-backed [`RepositoryDiscovery`].
//!
//! Discovery runs in three steps, each in its own module or function:
//! 1. `tree_walk` inventories manifests, sources, and compiler configs under budget.
//! 2. Manifest adapters turn each manifest into packages and workspace declarations.
//! 3. `workspace_membership` and `assemble` combine them into a [`RepositoryStructure`].
mod contained_fs;
mod tree_walk;
mod workspace_membership;

use std::{collections::BTreeSet, path::Path, sync::Arc};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

pub use contained_fs::{read_contained_directory, read_contained_file, DirectoryEntry};
use tree_walk::{scan_tree, TreeScan, DEFAULT_SOURCE_EXTENSIONS};

use super::{
    contracts::{ManifestAdapter, ManifestInput, RepositoryDiscovery, WorkspaceDeclaration},
    ids::{PackageId, RepositoryId, RootRelativePath},
    manifests::default_manifest_adapters,
    model::{
        DiscoveryDiagnostic, DiscoveryError, DiscoveryLimits, ModuleComponent, Package,
        RepositoryStructure,
    },
};

/// Filesystem scanner; symlinks and common generated/vendor directories are skipped.
#[derive(Clone)]
pub struct FsRepositoryDiscovery {
    adapters: Vec<Arc<dyn ManifestAdapter>>,
    source_extensions: Vec<String>,
}

impl Default for FsRepositoryDiscovery {
    fn default() -> Self {
        Self::with_adapters(default_manifest_adapters())
    }
}

impl FsRepositoryDiscovery {
    /// Discovery that understands exactly the given manifest formats.
    pub fn with_adapters(adapters: Vec<Arc<dyn ManifestAdapter>>) -> Self {
        Self {
            adapters,
            source_extensions: DEFAULT_SOURCE_EXTENSIONS
                .iter()
                .map(|e| (*e).to_owned())
                .collect(),
        }
    }

    /// Replaces the file extensions inventoried as source files (without the dot).
    pub fn with_source_extensions(mut self, extensions: Vec<String>) -> Self {
        self.source_extensions = extensions;
        self
    }

    /// Synchronous scan of `root`; [`RepositoryDiscovery::discover`] wraps this.
    pub fn scan(
        &self,
        root: &Path,
        limits: &DiscoveryLimits,
        cancel: &CancellationToken,
    ) -> Result<RepositoryStructure, DiscoveryError> {
        let root = canonical_directory(root)?;
        let is_manifest = |name: &str| self.adapters.iter().any(|adapter| adapter.handles(name));
        let tree = scan_tree(&root, limits, &is_manifest, &self.source_extensions, cancel)?;
        let manifests = self.read_manifests(&root, &tree, limits, cancel)?;
        Ok(self.assemble(&root, limits, tree, manifests))
    }

    fn adapter_for(&self, path: &RootRelativePath) -> Option<(usize, &Arc<dyn ManifestAdapter>)> {
        let name = path.as_path().file_name()?.to_str()?;
        self.adapters
            .iter()
            .enumerate()
            .find(|(_, adapter)| adapter.handles(name))
    }

    /// Reads and parses every inventoried manifest; unreadable ones become diagnostics.
    fn read_manifests(
        &self,
        root: &Path,
        tree: &TreeScan,
        limits: &DiscoveryLimits,
        cancel: &CancellationToken,
    ) -> Result<ParsedManifests, DiscoveryError> {
        let mut parsed = ParsedManifests::default();
        for file in &tree.manifests {
            if cancel.is_cancelled() {
                return Err(DiscoveryError::Cancelled);
            }
            let path = RootRelativePath::new(file)?;
            let Some((adapter_index, adapter)) = self.adapter_for(&path) else {
                continue;
            };
            let text = match read_manifest_text(root, &path, limits) {
                Ok(text) => text,
                Err(failure) => {
                    parsed.omitted_entries += usize::from(failure.exceeded_limit);
                    parsed.diagnostics.push(failure.diagnostic);
                    continue;
                }
            };
            let result = adapter.parse(&ManifestInput {
                root,
                path: &path,
                text: &text,
            });
            parsed.packages.extend(result.packages);
            parsed.diagnostics.extend(result.diagnostics);
            if let Some(declaration) = result.workspace {
                parsed.declarations.push((adapter_index, declaration));
            }
        }
        Ok(parsed)
    }

    fn assemble(
        &self,
        root: &Path,
        limits: &DiscoveryLimits,
        tree: TreeScan,
        mut manifests: ParsedManifests,
    ) -> RepositoryStructure {
        self.share_workspace_members(&mut manifests);
        let mut packages = manifests.packages;
        packages.sort_by(|a, b| a.manifest.cmp(&b.manifest));
        let mut diagnostics = tree.diagnostics;
        diagnostics.extend(manifests.diagnostics);
        diagnostics.extend(duplicate_name_diagnostics(&packages));
        diagnostics.sort_by(|a, b| a.path.cmp(&b.path).then(a.message.cmp(&b.message)));
        let workspace_members = manifests
            .declarations
            .iter()
            .flat_map(|(_, declaration)| declaration.members.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        RepositoryStructure {
            id: RepositoryId(root.to_string_lossy().into_owned()),
            modules: module_components(&packages),
            packages,
            workspace_members,
            diagnostics,
            effective_limits: limits.clone(),
            omitted_entries: tree.omitted_entries + manifests.omitted_entries,
            source_files: relative_paths(&tree.sources),
            config_files: relative_paths(&tree.configs),
        }
    }

    /// Lets packages inherit the members of an enclosing workspace, per manifest format.
    fn share_workspace_members(&self, manifests: &mut ParsedManifests) {
        for (index, adapter) in self.adapters.iter().enumerate() {
            if !adapter.inherits_workspace_members() {
                continue;
            }
            let declarations = manifests
                .declarations
                .iter()
                .filter(|(owner, _)| *owner == index)
                .map(|(_, declaration)| declaration.clone())
                .collect::<Vec<_>>();
            workspace_membership::apply_inherited_members(
                &mut manifests.packages,
                |package| {
                    package
                        .manifest
                        .as_path()
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| adapter.handles(name))
                },
                &declarations,
            );
        }
        for package in &mut manifests.packages {
            package.facts.workspace_members.sort();
            package.facts.workspace_members.dedup();
        }
    }
}

#[async_trait]
impl RepositoryDiscovery for FsRepositoryDiscovery {
    async fn discover(
        &self,
        root: &Path,
        limits: &DiscoveryLimits,
        cancel: CancellationToken,
    ) -> Result<RepositoryStructure, DiscoveryError> {
        self.scan(root, limits, &cancel)
    }
}

#[derive(Default)]
struct ParsedManifests {
    packages: Vec<Package>,
    /// Declaring adapter index and its workspace declaration.
    declarations: Vec<(usize, WorkspaceDeclaration)>,
    diagnostics: Vec<DiscoveryDiagnostic>,
    omitted_entries: usize,
}

struct ManifestReadFailure {
    diagnostic: DiscoveryDiagnostic,
    exceeded_limit: bool,
}

fn read_manifest_text(
    root: &Path,
    path: &RootRelativePath,
    limits: &DiscoveryLimits,
) -> Result<String, ManifestReadFailure> {
    let failure = |message: String, exceeded_limit| ManifestReadFailure {
        diagnostic: DiscoveryDiagnostic::for_path(path, message),
        exceeded_limit,
    };
    let bytes =
        read_contained_file(root, path.as_path(), limits.max_file_bytes).map_err(|error| {
            match error.kind() {
                std::io::ErrorKind::FileTooLarge => {
                    failure("manifest exceeds configured byte limit".into(), true)
                }
                _ => failure(error.to_string(), false),
            }
        })?;
    String::from_utf8(bytes).map_err(|error| failure(error.to_string(), false))
}

fn canonical_directory(root: &Path) -> Result<std::path::PathBuf, DiscoveryError> {
    let root = std::fs::canonicalize(root).map_err(|e| DiscoveryError::Failed(e.to_string()))?;
    if root.is_dir() {
        Ok(root)
    } else {
        Err(DiscoveryError::Failed(
            "authorized root is not a directory".into(),
        ))
    }
}

fn duplicate_name_diagnostics(packages: &[Package]) -> Vec<DiscoveryDiagnostic> {
    let mut seen = BTreeSet::new();
    packages
        .iter()
        .filter(|package| !seen.insert(package.name.clone()))
        .map(|package| {
            DiscoveryDiagnostic::for_path(
                &package.manifest,
                format!("duplicate package name: {}", package.name),
            )
        })
        .collect()
}

fn module_components(packages: &[Package]) -> Vec<ModuleComponent> {
    packages
        .iter()
        .map(|package| ModuleComponent {
            path: package.manifest.clone(),
            package: PackageId(package.id.0.clone()),
        })
        .collect()
}

fn relative_paths(paths: &[std::path::PathBuf]) -> Vec<RootRelativePath> {
    paths
        .iter()
        .filter_map(|path| RootRelativePath::new(path).ok())
        .collect()
}
