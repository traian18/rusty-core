//! Refreshing the graph: discovery, then imports, groups and frameworks, each cached by snapshot.

use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::dependency_groups::{GroupIndex, GroupInput};
use crate::framework_relations::FrameworkIndex;
use crate::import_graph::{ImportBuild, ImportIndex};
use crate::repository_intelligence::{
    ids::SnapshotId,
    metrics::RefreshMetrics,
    model::{DiscoveryError, RepositorySnapshot, RepositoryStructure},
};

use super::RepositoryGraphService;

impl RepositoryGraphService {
    pub(super) async fn acquire_refresh_lock(
        &self,
        cancel: &CancellationToken,
    ) -> Result<tokio::sync::MutexGuard<'_, ()>, DiscoveryError> {
        if cancel.is_cancelled() {
            return Err(DiscoveryError::Cancelled);
        }
        let guard = cancel
            .run_until_cancelled(self.refresh_lock.lock())
            .await
            .ok_or(DiscoveryError::Cancelled)?;
        if cancel.is_cancelled() {
            return Err(DiscoveryError::Cancelled);
        }
        Ok(guard)
    }

    /// Builds the import index off the async runtime; returns an empty index without support.
    pub(super) async fn index_imports(
        &self,
        structure: &RepositoryStructure,
        cancel: &CancellationToken,
    ) -> Result<ImportBuild, DiscoveryError> {
        if self.imports.is_empty() {
            return Ok(ImportBuild {
                index: ImportIndex::default(),
                metrics: Default::default(),
            });
        }
        let pipeline = self.imports.clone();
        let root = self.root.clone();
        let (structure, token) = (structure.clone(), cancel.clone());
        tokio::task::spawn_blocking(move || {
            let root = std::fs::canonicalize(root)
                .map_err(|error| DiscoveryError::Failed(error.to_string()))?;
            pipeline.build(&root, &structure, &token)
        })
        .await
        .map_err(|error| DiscoveryError::Failed(error.to_string()))?
    }

    /// Runs the group analyzer, if one is registered, over the finished structure and import index.
    pub(super) async fn analyze_groups(
        &self,
        structure: &RepositoryStructure,
        imports: &ImportIndex,
        cancel: &CancellationToken,
    ) -> Result<GroupIndex, DiscoveryError> {
        let Some(analyzer) = self.group_analyzer.clone() else {
            return Ok(GroupIndex::default());
        };
        let (structure, imports, token) = (structure.clone(), imports.clone(), cancel.clone());
        tokio::task::spawn_blocking(move || {
            let limits = analyzer.limits();
            analyzer.analyze(
                &GroupInput {
                    structure: &structure,
                    imports: &imports,
                    limits: &limits,
                },
                &token,
            )
        })
        .await
        .map_err(|error| DiscoveryError::Failed(error.to_string()))?
    }

    /// Runs the framework analyzer, if one is registered, over the finished structure and imports.
    pub(super) async fn analyze_frameworks(
        &self,
        structure: &RepositoryStructure,
        imports: &ImportIndex,
        cancel: &CancellationToken,
    ) -> Result<FrameworkIndex, DiscoveryError> {
        let Some(analyzer) = self.framework_analyzer.clone() else {
            return Ok(FrameworkIndex::default());
        };
        let root = self.root.clone();
        let (structure, imports, token) = (structure.clone(), imports.clone(), cancel.clone());
        tokio::task::spawn_blocking(move || {
            let root = std::fs::canonicalize(root)
                .map_err(|error| DiscoveryError::Failed(error.to_string()))?;
            analyzer.analyze(&root, &structure, &imports, &token)
        })
        .await
        .map_err(|error| DiscoveryError::Failed(error.to_string()))?
    }
}

/// Folds the import index into the structural snapshot id.
pub(super) fn combine_with_imports(structural: SnapshotId, imports: &ImportIndex) -> SnapshotId {
    let input = format!("imports-v1:{}:{}", structural.0, imports.digest);
    SnapshotId(format!("{:x}", Sha256::digest(input)))
}

/// Folds the framework relations (and so the adapter versions) into the snapshot id.
pub(super) fn combine_with_frameworks(base: SnapshotId, frameworks: &FrameworkIndex) -> SnapshotId {
    let input = format!("frameworks-v1:{}:{}", base.0, frameworks.digest);
    SnapshotId(format!("{:x}", Sha256::digest(input)))
}

/// Folds the group index (and so the rule versions) into the snapshot id.
pub(super) fn combine_with_groups(base: SnapshotId, groups: &GroupIndex) -> SnapshotId {
    let input = format!("groups-v1:{}:{}", base.0, groups.digest);
    SnapshotId(format!("{:x}", Sha256::digest(input)))
}

/// Emits one structured event per refresh: counts and durations only.
pub(super) fn log_refresh(snapshot: &RepositorySnapshot, metrics: &RefreshMetrics) {
    let details = metrics.import_details.clone().unwrap_or_default();
    tracing::info!(
        target: "repository_graph",
        packages = snapshot.graph.packages.len(),
        files = details.files,
        source_bytes = details.source_bytes,
        facts = details.facts,
        resolved_internal = details.resolved_internal,
        declared_external = details.declared_external,
        possible = details.possible,
        unresolved = details.unresolved,
        dynamic_or_unsupported = details.dynamic_or_unsupported,
        diagnostics = snapshot.graph.diagnostics.len() + details.diagnostics,
        partial = details.partial,
        cache_hits = details.cache.hits,
        cache_misses = details.cache.misses,
        discovery_ms = metrics.discovery.as_millis() as u64,
        identity_ms = metrics.identity.as_millis() as u64,
        extraction_ms = details.durations.extraction.as_millis() as u64,
        resolution_ms = details.durations.resolution.as_millis() as u64,
        publication_ms = metrics.publication.as_millis() as u64,
        total_ms = metrics.total.as_millis() as u64,
        "repository graph refreshed"
    );
}
