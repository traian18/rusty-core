//! [`RepositoryGraph`] for the production service: refresh, then read-only queries over the snapshot.
use std::{sync::Arc, time::Instant};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::dependency_groups::{
    BoundaryQuery, GraphProjection, GraphQuery, GroupAssignment, GroupListQuery, GroupMembersQuery,
    GroupOverview, GroupPage, GroupQueryError, MemberPage, OverviewQuery,
};
use crate::framework_relations::{
    FrameworkCapability, FrameworkPage, FrameworkQuery, FrameworkQueryError,
};
use crate::import_graph::{
    Direction, ImportCapabilities, ImportPage, ImportQuery, ImportQueryError, ImportSupport,
    LanguageCapability,
};
use crate::repository_intelligence::{
    contracts::RepositoryGraph,
    ids::{RootRelativePath, SnapshotId},
    metrics::RefreshMetrics,
    model::{DiscoveryCapabilities, DiscoveryError, RepositorySnapshot},
    snapshot_identity::snapshot_id,
};

use super::{
    refresh::{combine_with_frameworks, combine_with_groups, combine_with_imports, log_refresh},
    RepositoryGraphService,
};

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

    fn classify(&self, path: &RootRelativePath) -> Option<crate::import_graph::LanguageProfile> {
        self.imports
            .supports()
            .iter()
            .find_map(|support| support.adapter.profile_for(path))
    }

    fn read_file(&self, path: &RootRelativePath, max_bytes: u64) -> std::io::Result<Vec<u8>> {
        crate::repository_intelligence::read_contained_file(&self.root, path.as_path(), max_bytes)
    }

    fn path_state(&self, path: &RootRelativePath) -> crate::repository_intelligence::PathState {
        use crate::repository_intelligence::PathState;
        let relative = path.as_path();
        match crate::repository_intelligence::read_contained_directory(&self.root, relative) {
            Ok(_) => return PathState::Directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return PathState::Missing
            }
            Err(_) => {}
        }
        match crate::repository_intelligence::read_contained_file(
            &self.root,
            relative,
            self.limits.max_file_bytes,
        ) {
            Ok(bytes) => PathState::File {
                content_hash: crate::repository_intelligence::content_hash(&bytes),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => PathState::Missing,
            Err(_) => PathState::Unknown,
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
