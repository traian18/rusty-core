//! Composition ([`RepositoryGraphBuilder`]) and the production [`RepositoryGraph`].
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::{
    contracts::{
        ManifestAdapter, RepositoryDiscovery, RepositoryGraph, RepositorySnapshotRepository,
    },
    discovery::FsRepositoryDiscovery,
    ids::{RootRelativePath, SnapshotId},
    manifests::default_manifest_adapters,
    metrics::RefreshMetrics,
    model::{
        DiscoveryCapabilities, DiscoveryError, DiscoveryLimits, RepositorySnapshot,
        RepositoryStructure,
    },
    snapshot_identity::snapshot_id,
    snapshot_store::InMemoryRepositorySnapshotRepository,
};
use crate::dependency_groups::{
    BoundaryQuery, DependencyGroupAnalyzer, GraphProjection, GraphQuery, GroupAssignment,
    GroupIndex, GroupInput, GroupListQuery, GroupMembersQuery, GroupOverview, GroupPage,
    GroupQueries, GroupQueryError, MemberPage, OverviewQuery,
};
use crate::framework_relations::{
    DefaultFrameworkAnalyzer, FrameworkAdapter, FrameworkAnalyzer, FrameworkCapability,
    FrameworkIndex, FrameworkLimits, FrameworkPage, FrameworkQuery, FrameworkQueryError,
};
use crate::import_graph::{
    CacheStats, Direction, ImportBuild, ImportCapabilities, ImportIndex, ImportLimits, ImportPage,
    ImportPipeline, ImportQuery, ImportQueryError, ImportSupport, LanguageCapability,
};

const DEFAULT_SNAPSHOT_RETENTION: usize = 4;

/// Validated repository graph composition for one authorized workspace root.
pub struct RepositoryGraphBuilder {
    root: PathBuf,
    limits: DiscoveryLimits,
    discovery: Option<Arc<dyn RepositoryDiscovery>>,
    manifest_adapters: Vec<Arc<dyn ManifestAdapter>>,
    source_extensions: Option<Vec<String>>,
    snapshots: Arc<dyn RepositorySnapshotRepository>,
    import_supports: Vec<ImportSupport>,
    import_limits: ImportLimits,
    group_analyzer: Option<Arc<dyn DependencyGroupAnalyzer>>,
    framework_analyzer: Option<Arc<dyn FrameworkAnalyzer>>,
    framework_adapters: Vec<Arc<dyn FrameworkAdapter>>,
}

impl RepositoryGraphBuilder {
    /// Starts a composition for `root`, which must be absolute and already authorized.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, DiscoveryError> {
        let root = root.into();
        if !root.is_absolute() {
            return Err(DiscoveryError::Failed(
                "authorized root must be absolute".into(),
            ));
        }
        Ok(Self {
            root,
            limits: DiscoveryLimits::default(),
            discovery: None,
            manifest_adapters: default_manifest_adapters(),
            source_extensions: None,
            snapshots: Arc::new(InMemoryRepositorySnapshotRepository::new(
                DEFAULT_SNAPSHOT_RETENTION,
            )?),
            import_supports: Vec::new(),
            import_limits: ImportLimits::default(),
            group_analyzer: None,
            framework_analyzer: None,
            framework_adapters: Vec::new(),
        })
    }

    pub fn limits(mut self, limits: DiscoveryLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Replaces the whole discovery implementation (manifest adapters are then unused).
    pub fn discovery(mut self, discovery: Arc<dyn RepositoryDiscovery>) -> Self {
        self.discovery = Some(discovery);
        self
    }

    /// Replaces the manifest formats the filesystem discovery understands.
    pub fn manifest_adapters(mut self, adapters: Vec<Arc<dyn ManifestAdapter>>) -> Self {
        self.manifest_adapters = adapters;
        self
    }

    /// Replaces the source file extensions inventoried (without the dot); languages register theirs.
    pub fn source_extensions(mut self, extensions: Vec<String>) -> Self {
        self.source_extensions = Some(extensions);
        self
    }

    pub fn snapshot_repository(mut self, snapshots: Arc<dyn RepositorySnapshotRepository>) -> Self {
        self.snapshots = snapshots;
        self
    }

    /// Limits applied to import extraction, resolution, and paging.
    pub fn import_limits(mut self, limits: ImportLimits) -> Self {
        self.import_limits = limits;
        self
    }

    /// Registers a language's adapter and resolver. Two supports may not resolve the same profile.
    pub fn import_support(mut self, support: ImportSupport) -> Result<Self, DiscoveryError> {
        let profiles = support.resolver.profiles();
        let taken = self.import_supports.iter().any(|existing| {
            let existing = existing.resolver.profiles();
            existing.iter().any(|profile| profiles.contains(profile))
        });
        if taken {
            return Err(DiscoveryError::Failed(
                "an import resolver is already registered for one of these profiles".into(),
            ));
        }
        self.import_supports.push(support);
        Ok(self)
    }

    /// Enables dependency groups, cycles, and graph projections over the import index.
    pub fn dependency_groups(mut self, analyzer: Arc<dyn DependencyGroupAnalyzer>) -> Self {
        self.group_analyzer = Some(analyzer);
        self
    }

    /// Registers a framework adapter. Adapters run only for packages where they can show the
    /// framework applies; with none registered no relations are computed.
    pub fn framework_adapter(mut self, adapter: Arc<dyn FrameworkAdapter>) -> Self {
        self.framework_adapters.push(adapter);
        self
    }

    /// Replaces the framework analyzer (the default runs the registered adapters).
    pub fn framework_analyzer(mut self, analyzer: Arc<dyn FrameworkAnalyzer>) -> Self {
        self.framework_analyzer = Some(analyzer);
        self
    }

    pub fn build(self) -> RepositoryGraphService {
        let framework_analyzer = self.framework_analyzer.clone().or_else(|| {
            (!self.framework_adapters.is_empty()).then(|| {
                Arc::new(DefaultFrameworkAnalyzer::new(
                    self.framework_adapters.clone(),
                    FrameworkLimits::default(),
                )) as Arc<dyn FrameworkAnalyzer>
            })
        });
        let framework_capabilities = self
            .framework_adapters
            .iter()
            .map(|adapter| adapter.capability())
            .collect();
        let discovery = self.discovery.unwrap_or_else(|| {
            let mut discovery = FsRepositoryDiscovery::with_adapters(self.manifest_adapters);
            if let Some(extensions) = self.source_extensions {
                discovery = discovery.with_source_extensions(extensions);
            }
            Arc::new(discovery)
        });
        RepositoryGraphService {
            root: self.root,
            limits: self.limits,
            discovery,
            snapshots: self.snapshots,
            imports: Arc::new(ImportPipeline::new(
                self.import_supports,
                self.import_limits,
            )),
            group_analyzer: self.group_analyzer,
            framework_analyzer,
            framework_capabilities,
            refresh_lock: tokio::sync::Mutex::new(()),
            last_metrics: Mutex::new(None),
        }
    }
}

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

    async fn acquire_refresh_lock(
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
    async fn index_imports(
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
    async fn analyze_groups(
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
    async fn analyze_frameworks(
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

    /// Runs `ask` against the named snapshot's groups and imports.
    fn with_group_queries<T>(
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

    fn query_imports(
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

/// Folds the import index into the structural snapshot id.
fn combine_with_imports(structural: SnapshotId, imports: &ImportIndex) -> SnapshotId {
    let input = format!("imports-v1:{}:{}", structural.0, imports.digest);
    SnapshotId(format!("{:x}", Sha256::digest(input)))
}

/// Folds the framework relations (and so the adapter versions) into the snapshot id.
fn combine_with_frameworks(base: SnapshotId, frameworks: &FrameworkIndex) -> SnapshotId {
    let input = format!("frameworks-v1:{}:{}", base.0, frameworks.digest);
    SnapshotId(format!("{:x}", Sha256::digest(input)))
}

/// Folds the group index (and so the rule versions) into the snapshot id.
fn combine_with_groups(base: SnapshotId, groups: &GroupIndex) -> SnapshotId {
    let input = format!("groups-v1:{}:{}", base.0, groups.digest);
    SnapshotId(format!("{:x}", Sha256::digest(input)))
}

#[async_trait]
impl RepositoryGraph for RepositoryGraphService {
    async fn refresh(
        &self,
        cancel: CancellationToken,
    ) -> Result<RepositorySnapshot, DiscoveryError> {
        let _guard = self.acquire_refresh_lock(&cancel).await?;
        let started = Instant::now();
        let graph = self
            .discovery
            .discover(&self.root, &self.limits, cancel.clone())
            .await?;
        let discovered = started.elapsed();
        let structural_id = snapshot_id(&graph, &self.root)?;
        let identified = started.elapsed();
        let build = self.index_imports(&graph, &cancel).await?;
        let indexed = started.elapsed();
        if cancel.is_cancelled() {
            return Err(DiscoveryError::Cancelled);
        }
        let has_imports = !self.imports.is_empty();
        let groups = self.analyze_groups(&graph, &build.index, &cancel).await?;
        let grouped = started.elapsed();
        if cancel.is_cancelled() {
            return Err(DiscoveryError::Cancelled);
        }
        let frameworks = self
            .analyze_frameworks(&graph, &build.index, &cancel)
            .await?;
        let framework_time = started.elapsed();
        if cancel.is_cancelled() {
            return Err(DiscoveryError::Cancelled);
        }
        let mut id = if has_imports {
            combine_with_imports(structural_id, &build.index)
        } else {
            structural_id
        };
        if groups.configured {
            id = combine_with_groups(id, &groups);
        }
        if frameworks.configured {
            id = combine_with_frameworks(id, &frameworks);
        }
        let snapshot = RepositorySnapshot {
            id,
            graph,
            imports: build.index,
            groups,
            frameworks,
        };
        self.snapshots.publish(snapshot.clone())?;
        let metrics = RefreshMetrics {
            discovery: discovered,
            identity: identified - discovered,
            imports: indexed - identified,
            groups: grouped - indexed,
            frameworks: framework_time - grouped,
            publication: started.elapsed() - framework_time,
            total: started.elapsed(),
            import_details: has_imports.then_some(build.metrics),
        };
        log_refresh(&snapshot, &metrics);
        if let Ok(mut last) = self.last_metrics.lock() {
            *last = Some(metrics);
        }
        Ok(snapshot)
    }

    fn capabilities(&self) -> DiscoveryCapabilities {
        DiscoveryCapabilities {
            cargo: true,
            package_json: true,
            symlink_safe: cfg!(unix),
        }
    }

    fn current(&self) -> Option<Arc<RepositorySnapshot>> {
        self.snapshots.current()
    }

    fn get(&self, id: &SnapshotId) -> Result<Arc<RepositorySnapshot>, DiscoveryError> {
        self.snapshots.get(id)
    }

    fn import_capabilities(&self) -> ImportCapabilities {
        ImportCapabilities {
            languages: self
                .imports
                .supports()
                .iter()
                .map(language_capability)
                .collect(),
            limits: self.imports.limits().clone(),
        }
    }

    fn dependencies(&self, query: &ImportQuery) -> Result<ImportPage, ImportQueryError> {
        self.query_imports(Direction::Dependencies, query)
    }

    fn dependents(&self, query: &ImportQuery) -> Result<ImportPage, ImportQueryError> {
        self.query_imports(Direction::Dependents, query)
    }

    fn groups(&self, query: &GroupListQuery) -> Result<GroupPage, GroupQueryError> {
        self.with_group_queries(&query.snapshot, |queries| queries.list(query))
    }

    fn group_members(&self, query: &GroupMembersQuery) -> Result<MemberPage, GroupQueryError> {
        self.with_group_queries(&query.snapshot, |queries| queries.members(query))
    }

    fn groups_of(
        &self,
        snapshot: &SnapshotId,
        file: &RootRelativePath,
    ) -> Result<Vec<GroupAssignment>, GroupQueryError> {
        self.with_group_queries(snapshot, |queries| queries.assignments(file))
    }

    fn boundary_edges(&self, query: &BoundaryQuery) -> Result<ImportPage, GroupQueryError> {
        self.with_group_queries(&query.snapshot, |queries| queries.boundary_edges(query))
    }

    fn group_overview(&self, query: &OverviewQuery) -> Result<GroupOverview, GroupQueryError> {
        self.with_group_queries(&query.snapshot, |queries| queries.overview(query))
    }

    fn framework_relations(
        &self,
        query: &FrameworkQuery,
    ) -> Result<FrameworkPage, FrameworkQueryError> {
        let snapshot = self
            .snapshots
            .get(&query.snapshot)
            .map_err(|_| FrameworkQueryError::SnapshotNotFound(query.snapshot.0.clone()))?;
        snapshot.frameworks.query(&snapshot.id, query)
    }

    fn framework_capabilities(&self) -> Vec<FrameworkCapability> {
        self.framework_capabilities.clone()
    }

    fn graph(&self, query: &GraphQuery) -> Result<GraphProjection, GroupQueryError> {
        self.with_group_queries(&query.snapshot, |queries| queries.graph(query))
    }
}

fn language_capability(support: &ImportSupport) -> LanguageCapability {
    let owned = |names: Vec<&'static str>| names.into_iter().map(str::to_owned).collect();
    LanguageCapability {
        profiles: support.resolver.profiles(),
        adapter: support.adapter.id().to_owned(),
        adapter_version: support.adapter.version(),
        resolver: support.resolver.id().to_owned(),
        resolver_version: support.resolver.version(),
        forms: support.adapter.forms(),
        resolution_rules: owned(support.resolver.rules()),
        unsupported: owned(support.resolver.unsupported()),
    }
}

/// Emits one structured event per refresh: counts and durations only.
fn log_refresh(snapshot: &RepositorySnapshot, metrics: &RefreshMetrics) {
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
