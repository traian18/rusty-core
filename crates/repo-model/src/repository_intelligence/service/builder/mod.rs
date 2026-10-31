//! Composing a [`RepositoryGraphService`] from its parts.
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

mod analysis;

use super::{RepositoryGraphService, DEFAULT_SNAPSHOT_RETENTION};

use crate::dependency_groups::DependencyGroupAnalyzer;
use crate::framework_relations::{
    DefaultFrameworkAnalyzer, FrameworkAdapter, FrameworkAnalyzer, FrameworkLimits,
};
use crate::import_graph::{ImportLimits, ImportPipeline, ImportSupport};
use crate::repository_intelligence::{
    contracts::{ManifestAdapter, RepositoryDiscovery, RepositorySnapshotRepository},
    discovery::FsRepositoryDiscovery,
    manifests::default_manifest_adapters,
    model::{DiscoveryError, DiscoveryLimits},
    snapshot_store::InMemoryRepositorySnapshotRepository,
};

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
