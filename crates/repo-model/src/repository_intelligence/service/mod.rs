//! Composition ([`RepositoryGraphBuilder`]) and the production [`RepositoryGraph`].

mod builder;
mod graph;
mod refresh;

pub use builder::RepositoryGraphBuilder;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use crate::dependency_groups::{DependencyGroupAnalyzer, GroupQueries, GroupQueryError};
use crate::framework_relations::{FrameworkAnalyzer, FrameworkCapability};
use crate::import_graph::{
    CacheStats, Direction, ImportPage, ImportPipeline, ImportQuery, ImportQueryError,
};
use crate::repository_intelligence::{
    contracts::{RepositoryDiscovery, RepositorySnapshotRepository},
    ids::SnapshotId,
    metrics::RefreshMetrics,
    model::DiscoveryLimits,
};

pub(super) const DEFAULT_SNAPSHOT_RETENTION: usize = 4;

/// Production repository graph service. Refreshes are explicit and atomically published.
pub struct RepositoryGraphService {
    root: PathBuf,
    limits: DiscoveryLimits,
    discovery: Arc<dyn RepositoryDiscovery>,
    snapshots: Arc<dyn RepositorySnapshotRepository>,
    imports: Arc<ImportPipeline>,
    group_analyzer: Option<Arc<dyn DependencyGroupAnalyzer>>,
    framework_analyzer: Option<Arc<dyn FrameworkAnalyzer>>,
    framework_capabilities: Vec<FrameworkCapability>,
    refresh_lock: tokio::sync::Mutex<()>,
    last_metrics: Mutex<Option<RefreshMetrics>>,
}

impl RepositoryGraphService {
    /// Timings and counts of the most recent successful refresh.
    pub fn last_refresh_metrics(&self) -> Option<RefreshMetrics> {
        self.last_metrics
            .lock()
            .ok()
            .and_then(|metrics| metrics.clone())
    }

    /// Cumulative extraction-cache hits and misses since this service was built.
    pub fn import_cache_stats(&self) -> CacheStats {
        self.imports.cache_stats()
    }

    /// Runs `ask` against the named snapshot's groups and imports.
    pub(super) fn with_group_queries<T>(
        &self,
        snapshot_id: &SnapshotId,
        ask: impl FnOnce(&GroupQueries<'_>) -> Result<T, GroupQueryError>,
    ) -> Result<T, GroupQueryError> {
        let snapshot = self
            .snapshots
            .get(snapshot_id)
            .map_err(|_| GroupQueryError::SnapshotNotFound(snapshot_id.0.clone()))?;
        ask(&GroupQueries {
            snapshot: &snapshot.id,
            groups: &snapshot.groups,
            imports: &snapshot.imports,
        })
    }

    pub(super) fn query_imports(
        &self,
        direction: Direction,
        query: &ImportQuery,
    ) -> Result<ImportPage, ImportQueryError> {
        let snapshot = self
            .snapshots
            .get(&query.snapshot)
            .map_err(|_| ImportQueryError::SnapshotNotFound(query.snapshot.0.clone()))?;
        snapshot.imports.query(&snapshot.id, direction, query)
    }
}
