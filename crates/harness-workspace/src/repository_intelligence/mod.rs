//! Repository structure discovery and the published snapshot graph.
//!
//! # Layers
//!
//! | Module | Responsibility |
//! |---|---|
//! | `ids`, `model` | Plain data: identities, packages, limits, failures |
//! | `contracts` | Traits consumers and composition code depend on |
//! | `discovery` | Filesystem scan: bounded traversal, contained reads, workspace membership |
//! | `manifests` | One adapter per manifest format (`Cargo.toml`, `package.json`) |
//! | `snapshot_identity` | Deterministic snapshot ids |
//! | `snapshot_store` | Bounded in-memory snapshot storage |
//! | `service` | Builder and the production [`RepositoryGraph`] |
//!
//! Consumers hold an `Arc<dyn RepositoryGraph>`; scanners, manifest adapters, and
//! snapshot stores are replaceable behind their traits. This module is
//! intentionally independent of source-level call-graph analysis.
mod contracts;
mod discovery;
mod ids;
mod manifests;
mod metrics;
mod model;
mod service;
mod snapshot_identity;
mod snapshot_store;

pub use contracts::{
    ManifestAdapter, ManifestInput, ManifestParse, RepositoryDiscovery, RepositoryGraph,
    RepositorySnapshotRepository, WorkspaceDeclaration,
};
pub use discovery::FsRepositoryDiscovery;
pub use ids::{PackageId, RepositoryId, RootRelativePath, SnapshotId};
pub use manifests::{default_manifest_adapters, CargoManifestAdapter, PackageJsonAdapter};
pub use metrics::RefreshMetrics;
pub use model::{
    DiscoveryCapabilities, DiscoveryDiagnostic, DiscoveryError, DiscoveryLimits, ModuleComponent,
    Package, PackageFacts, RepositorySnapshot, RepositoryStructure,
};
pub use service::{RepositoryGraphBuilder, RepositoryGraphService};
pub use snapshot_identity::snapshot_id;
pub use snapshot_store::InMemoryRepositorySnapshotRepository;

pub use discovery::{read_contained_directory, read_contained_file, DirectoryEntry};

#[cfg(test)]
mod tests;
