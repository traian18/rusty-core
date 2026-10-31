//! Plain data describing what discovery found: packages, workspaces, limits, and failures.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::ids::{PackageId, RepositoryId, RootRelativePath, SnapshotId};
use crate::{
    dependency_groups::GroupIndex, framework_relations::FrameworkIndex, import_graph::ImportIndex,
};

/// A recoverable issue observed while scanning or parsing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryDiagnostic {
    pub path: Option<RootRelativePath>,
    pub message: String,
}

impl DiscoveryDiagnostic {
    /// A diagnostic attached to one file.
    pub fn for_path(path: &RootRelativePath, message: impl Into<String>) -> Self {
        Self {
            path: Some(path.clone()),
            message: message.into(),
        }
    }
}

/// Scanner feature declarations.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryCapabilities {
    pub cargo: bool,
    pub package_json: bool,
    pub symlink_safe: bool,
}

/// Explicit resource limits for a discovery operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryLimits {
    pub max_entries: usize,
    pub max_file_bytes: u64,
    pub max_depth: usize,
}

impl Default for DiscoveryLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_file_bytes: 4 * 1024 * 1024,
            max_depth: 64,
        }
    }
}

/// Why discovery could not complete safely.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("invalid root-relative path: {0}")]
    InvalidRelativePath(PathBuf),
    #[error("discovery failed: {0}")]
    Failed(String),
    #[error("discovery cancelled")]
    Cancelled,
    #[error("snapshot not found: {0}")]
    SnapshotNotFound(String),
}

/// Provenance distinguishes manifest facts from future inference and source imports.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PackageFacts {
    /// Every declared dependency, whatever its kind.
    pub declared_dependencies: Vec<String>,
    /// Declared only as a dev, build, or peer dependency (never as a regular one); always a
    /// subset of `declared_dependencies`.
    #[serde(default)]
    pub dev_dependencies: Vec<String>,
    pub inferred_facts: Vec<String>,
    pub source_imports: Vec<String>,
    /// Root-relative Rust or JavaScript source entrypoints identified from manifests.
    pub source_roots: Vec<RootRelativePath>,
    /// Root-relative declared workspace member patterns/paths.
    pub workspace_members: Vec<RootRelativePath>,
    /// Declared (unverified) JavaScript entry files from `types`, `module`, `main`, and `exports`.
    pub entry_points: Vec<RootRelativePath>,
    /// Explicit Cargo target files from `[[bin]]`, `[[test]]`, `[[example]]`, and `[[bench]]`.
    pub target_roots: Vec<RootRelativePath>,
}

/// A discovered package and the manifest that declared it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Package {
    pub id: PackageId,
    pub name: String,
    pub manifest: RootRelativePath,
    pub facts: PackageFacts,
}

impl Package {
    /// A package with only manifest-declared identity and dependencies filled in.
    pub fn declared(
        name: String,
        manifest: RootRelativePath,
        declared_dependencies: Vec<String>,
    ) -> Self {
        let id = PackageId(format!("{}:{}", manifest.as_path().display(), name));
        Self {
            id,
            name,
            manifest,
            facts: PackageFacts {
                declared_dependencies,
                ..PackageFacts::default()
            },
        }
    }
}

/// A source/module component owned by a discovered package.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModuleComponent {
    pub path: RootRelativePath,
    pub package: PackageId,
}

/// Immutable result of repository structure discovery.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RepositoryStructure {
    pub id: RepositoryId,
    pub packages: Vec<Package>,
    pub modules: Vec<ModuleComponent>,
    /// Declared workspace member patterns/paths, including virtual-manifest workspaces.
    pub workspace_members: Vec<RootRelativePath>,
    pub diagnostics: Vec<DiscoveryDiagnostic>,
    pub effective_limits: DiscoveryLimits,
    pub omitted_entries: usize,
    /// Root-relative inventory of `.rs`, `.ts`, `.tsx`, `.mts`, `.cts`, `.js`, `.jsx`, `.mjs`, and `.cjs` files.
    #[serde(default)]
    pub source_files: Vec<RootRelativePath>,
    /// Root-relative `tsconfig*.json` and `jsconfig*.json` files used by import resolution.
    #[serde(default)]
    pub config_files: Vec<RootRelativePath>,
}

/// Immutable published repository generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RepositorySnapshot {
    pub id: SnapshotId,
    pub graph: RepositoryStructure,
    /// Empty unless import support is registered with the builder.
    #[serde(default)]
    pub imports: ImportIndex,
    /// Unconfigured unless an analyzer is registered with the builder.
    #[serde(default)]
    pub groups: GroupIndex,
    /// Unconfigured unless a framework analyzer is registered with the builder.
    #[serde(default)]
    pub frameworks: FrameworkIndex,
}
