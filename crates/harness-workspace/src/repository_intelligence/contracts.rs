//! Replaceable boundaries of the repository graph.
//!
//! Consumers depend on [`RepositoryGraph`]; composition code supplies the other
//! traits. Concrete scanners, parsers, and stores stay behind these interfaces.
use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::{
    ids::{RootRelativePath, SnapshotId},
    model::{
        DiscoveryCapabilities, DiscoveryDiagnostic, DiscoveryError, DiscoveryLimits, Package,
        RepositorySnapshot, RepositoryStructure,
    },
};
use crate::{
    dependency_groups::{
        BoundaryQuery, GraphProjection, GraphQuery, GroupAssignment, GroupListQuery,
        GroupMembersQuery, GroupOverview, GroupPage, GroupQueryError, MemberPage, OverviewQuery,
    },
    framework_relations::{
        FrameworkCapability, FrameworkPage, FrameworkQuery, FrameworkQueryError,
    },
    import_graph::{ImportCapabilities, ImportPage, ImportQuery, ImportQueryError},
};

/// Finds repositories, packages, and source files under an authorized root.
#[async_trait]
pub trait RepositoryDiscovery: Send + Sync {
    async fn discover(
        &self,
        root: &Path,
        limits: &DiscoveryLimits,
        cancel: CancellationToken,
    ) -> Result<RepositoryStructure, DiscoveryError>;
}

/// Stable consumer contract for one authorized repository context.
#[async_trait]
pub trait RepositoryGraph: Send + Sync {
    /// Scans the repository and publishes a new immutable snapshot.
    async fn refresh(
        &self,
        cancel: CancellationToken,
    ) -> Result<RepositorySnapshot, DiscoveryError>;
    fn capabilities(&self) -> DiscoveryCapabilities;
    /// The most recently published snapshot, if any.
    fn current(&self) -> Option<Arc<RepositorySnapshot>>;
    fn get(&self, id: &SnapshotId) -> Result<Arc<RepositorySnapshot>, DiscoveryError>;
    /// Languages, forms, rules, and known gaps of the registered import support.
    fn import_capabilities(&self) -> ImportCapabilities {
        ImportCapabilities::default()
    }
    /// Imports made by `query.file`, in source order.
    fn dependencies(&self, query: &ImportQuery) -> Result<ImportPage, ImportQueryError>;
    /// Imports that target `query.file` (optionally one item of it).
    fn dependents(&self, query: &ImportQuery) -> Result<ImportPage, ImportQueryError>;

    /// Lists dependency groups, optionally of one kind.
    fn groups(&self, _query: &GroupListQuery) -> Result<GroupPage, GroupQueryError> {
        Err(GroupQueryError::NotConfigured)
    }
    /// Lists the member files of one group.
    fn group_members(&self, _query: &GroupMembersQuery) -> Result<MemberPage, GroupQueryError> {
        Err(GroupQueryError::NotConfigured)
    }
    /// Explains every group a file belongs to.
    fn groups_of(
        &self,
        _snapshot: &SnapshotId,
        _file: &RootRelativePath,
    ) -> Result<Vec<GroupAssignment>, GroupQueryError> {
        Err(GroupQueryError::NotConfigured)
    }
    /// Imports crossing a group's boundary.
    fn boundary_edges(&self, _query: &BoundaryQuery) -> Result<ImportPage, GroupQueryError> {
        Err(GroupQueryError::NotConfigured)
    }
    /// Import counts between the groups of one kind.
    fn group_overview(&self, _query: &OverviewQuery) -> Result<GroupOverview, GroupQueryError> {
        Err(GroupQueryError::NotConfigured)
    }
    /// Framework relations (routes, injected providers, rendered components, commands).
    fn framework_relations(
        &self,
        _query: &FrameworkQuery,
    ) -> Result<FrameworkPage, FrameworkQueryError> {
        Err(FrameworkQueryError::NotConfigured)
    }
    /// What each registered framework adapter detects and leaves alone.
    fn framework_capabilities(&self) -> Vec<FrameworkCapability> {
        Vec::new()
    }
    /// A bounded file subgraph around chosen roots.
    fn graph(&self, _query: &GraphQuery) -> Result<GraphProjection, GroupQueryError> {
        Err(GroupQueryError::NotConfigured)
    }
}

/// Replaceable storage boundary for immutable repository generations.
pub trait RepositorySnapshotRepository: Send + Sync {
    fn current(&self) -> Option<Arc<RepositorySnapshot>>;
    fn publish(&self, snapshot: RepositorySnapshot) -> Result<(), DiscoveryError>;
    fn get(&self, id: &SnapshotId) -> Result<Arc<RepositorySnapshot>, DiscoveryError>;
}

/// One manifest file handed to a [`ManifestAdapter`].
pub struct ManifestInput<'a> {
    /// Canonical authorized root, for adapters that confirm files next to the manifest.
    pub root: &'a Path,
    pub path: &'a RootRelativePath,
    pub text: &'a str,
}

/// Members a manifest declares for the workspace it roots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceDeclaration {
    /// Directory of the declaring manifest, relative to the root.
    pub base: std::path::PathBuf,
    pub members: Vec<RootRelativePath>,
}

/// What a manifest told us. Parsing never executes the manifest or touches the network.
#[derive(Default)]
pub struct ManifestParse {
    pub packages: Vec<Package>,
    pub workspace: Option<WorkspaceDeclaration>,
    pub diagnostics: Vec<DiscoveryDiagnostic>,
}

/// Reads one manifest format (for example `Cargo.toml` or `package.json`).
pub trait ManifestAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    /// Exact file name this adapter handles (the default for [`ManifestAdapter::handles`]).
    fn file_name(&self) -> &'static str;
    /// Whether a file of this name is one of this adapter's manifests. Override for formats named
    /// by pattern, such as `*.csproj`.
    fn handles(&self, file_name: &str) -> bool {
        file_name == self.file_name()
    }
    /// Whether packages under a declared workspace inherit its member list.
    fn inherits_workspace_members(&self) -> bool {
        false
    }
    fn parse(&self, input: &ManifestInput<'_>) -> ManifestParse;
}
