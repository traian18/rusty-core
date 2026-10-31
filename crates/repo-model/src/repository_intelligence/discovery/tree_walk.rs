//! Bounded, deterministic directory traversal that inventories manifests, sources, and configs.
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use tokio_util::sync::CancellationToken;

use super::contained_fs::{read_contained_directory, DirectoryEntry};
use crate::repository_intelligence::{
    DiscoveryDiagnostic, DiscoveryError, DiscoveryLimits, RootRelativePath,
};

/// Directories that never contain first-party sources worth indexing.
const IGNORED_DIRECTORIES: [&str; 14] = [
    ".git",
    "node_modules",
    "target",
    ".hg",
    ".svn",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".gradle",
    "obj",
];
/// Source extensions indexed unless the composition registers others (languages add theirs).
pub(super) const DEFAULT_SOURCE_EXTENSIONS: [&str; 9] =
    ["rs", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// Everything the traversal found, each list sorted by path.
#[derive(Default)]
pub(super) struct TreeScan {
    pub manifests: Vec<PathBuf>,
    pub sources: Vec<PathBuf>,
    pub configs: Vec<PathBuf>,
    pub diagnostics: Vec<DiscoveryDiagnostic>,
    pub omitted_entries: usize,
}

/// Walks `root` depth-first in sorted order, honoring entry, depth, and cancellation limits.
pub(super) fn scan_tree(
    root: &Path,
    limits: &DiscoveryLimits,
    is_manifest: &dyn Fn(&str) -> bool,
    source_extensions: &[String],
    cancel: &CancellationToken,
) -> Result<TreeScan, DiscoveryError> {
    let mut scanner = TreeScanner {
        root,
        limits,
        is_manifest,
        source_extensions,
        cancel,
        visited: 0,
        scan: TreeScan::default(),
    };
    scanner.run()?;
    let mut scan = scanner.scan;
    scan.manifests.sort();
    scan.sources.sort();
    scan.configs.sort();
    Ok(scan)
}

enum FileRole {
    Manifest,
    Source,
    Config,
    Other,
}

struct TreeScanner<'a> {
    root: &'a Path,
    limits: &'a DiscoveryLimits,
    is_manifest: &'a dyn Fn(&str) -> bool,
    source_extensions: &'a [String],
    cancel: &'a CancellationToken,
    visited: usize,
    scan: TreeScan,
}

impl TreeScanner<'_> {
    fn run(&mut self) -> Result<(), DiscoveryError> {
        let mut pending = vec![(PathBuf::new(), 0usize)];
        while let Some((directory, depth)) = pending.pop() {
            self.check_cancelled()?;
            if depth > self.limits.max_depth {
                self.scan.omitted_entries += 1;
                continue;
            }
            let Some(entries) = self.list_directory(&directory)? else {
                continue;
            };
            let children = self.classify_entries(&directory, entries)?;
            pending.extend(children.into_iter().rev().map(|path| (path, depth + 1)));
        }
        Ok(())
    }

    fn check_cancelled(&self) -> Result<(), DiscoveryError> {
        if self.cancel.is_cancelled() {
            Err(DiscoveryError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Lists one directory; unreadable directories become diagnostics, unsupported platforms fail.
    fn list_directory(
        &mut self,
        directory: &Path,
    ) -> Result<Option<Vec<DirectoryEntry>>, DiscoveryError> {
        match read_contained_directory(self.root, directory) {
            Ok(entries) => Ok(Some(entries)),
            Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
                Err(DiscoveryError::Failed(error.to_string()))
            }
            Err(error) => {
                self.scan.diagnostics.push(DiscoveryDiagnostic {
                    path: non_empty_relative(directory),
                    message: error.to_string(),
                });
                Ok(None)
            }
        }
    }

    /// Records files by role and returns the sorted child directories still to visit.
    fn classify_entries(
        &mut self,
        directory: &Path,
        entries: Vec<DirectoryEntry>,
    ) -> Result<Vec<PathBuf>, DiscoveryError> {
        let mut entries = entries.into_iter();
        let mut children = Vec::new();
        loop {
            if self.visited >= self.limits.max_entries {
                if entries.next().is_some() {
                    self.scan.omitted_entries = self.scan.omitted_entries.saturating_add(1);
                }
                break;
            }
            let Some(entry) = entries.next() else { break };
            self.check_cancelled()?;
            self.visited += 1;
            if is_ignored_directory(&entry.name) {
                continue;
            }
            let child = directory.join(&entry.name);
            if entry.is_directory {
                children.push(child);
            } else if entry.is_regular_file {
                self.record_file(child, &entry.name);
            }
        }
        children.sort();
        if self.visited >= self.limits.max_entries && !children.is_empty() {
            self.scan.omitted_entries = self.scan.omitted_entries.saturating_add(children.len());
        }
        Ok(children)
    }

    fn record_file(&mut self, path: PathBuf, name: &OsStr) {
        match self.role_of(name) {
            FileRole::Manifest => self.scan.manifests.push(path),
            FileRole::Source => self.scan.sources.push(path),
            FileRole::Config => self.scan.configs.push(path),
            FileRole::Other => {}
        }
    }

    fn role_of(&self, name: &OsStr) -> FileRole {
        let text = name.to_str().unwrap_or("");
        if (self.is_manifest)(text) {
            FileRole::Manifest
        } else if has_source_extension(name, self.source_extensions) {
            FileRole::Source
        } else if is_compiler_config(text) {
            FileRole::Config
        } else {
            FileRole::Other
        }
    }
}

/// The ignored directory a path lies under, if any.
pub fn ignored_directory_in(path: &std::path::Path) -> Option<&'static str> {
    path.components().find_map(|component| {
        let name = component.as_os_str().to_str()?;
        IGNORED_DIRECTORIES
            .iter()
            .copied()
            .find(|ignored| *ignored == name)
    })
}

fn is_ignored_directory(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| IGNORED_DIRECTORIES.contains(&name))
}

fn has_source_extension(name: &OsStr, extensions: &[String]) -> bool {
    Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extensions.iter().any(|known| known == extension))
}

/// `tsconfig.json`, `tsconfig.base.json`, `jsconfig.json`, and similar.
fn is_compiler_config(name: &str) -> bool {
    (name.starts_with("tsconfig") || name.starts_with("jsconfig")) && name.ends_with(".json")
}

fn non_empty_relative(path: &Path) -> Option<RootRelativePath> {
    if path.as_os_str().is_empty() {
        None
    } else {
        RootRelativePath::new(path).ok()
    }
}
