//! Tests for discovery, manifest adapters, snapshot identity, and the graph service.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use super::*;

mod discovery;
mod identity;
mod manifests;
mod service;

/// Scans `root` with the default manifest adapters.
fn discover_filesystem(
    root: &Path,
    limits: &DiscoveryLimits,
    cancel: &CancellationToken,
) -> Result<RepositoryStructure, DiscoveryError> {
    FsRepositoryDiscovery::default().scan(root, limits, cancel)
}

struct Substitute;

#[async_trait]
impl RepositoryDiscovery for Substitute {
    async fn discover(
        &self,
        _: &Path,
        limits: &DiscoveryLimits,
        cancel: CancellationToken,
    ) -> Result<RepositoryStructure, DiscoveryError> {
        if cancel.is_cancelled() {
            return Err(DiscoveryError::Cancelled);
        }
        Ok(RepositoryStructure {
            id: RepositoryId("substitute".into()),
            packages: vec![],
            modules: vec![],
            workspace_members: vec![],
            diagnostics: vec![],
            effective_limits: limits.clone(),
            omitted_entries: 0,
            source_files: vec![],
            config_files: vec![],
        })
    }
}

struct SubstituteSnapshots {
    inner: InMemoryRepositorySnapshotRepository,
}

impl SubstituteSnapshots {
    fn new() -> Self {
        Self {
            inner: InMemoryRepositorySnapshotRepository::new(2).unwrap(),
        }
    }
}

impl RepositorySnapshotRepository for SubstituteSnapshots {
    fn current(&self) -> Option<Arc<RepositorySnapshot>> {
        self.inner.current()
    }

    fn publish(&self, snapshot: RepositorySnapshot) -> Result<(), DiscoveryError> {
        self.inner.publish(snapshot)
    }

    fn get(&self, id: &SnapshotId) -> Result<Arc<RepositorySnapshot>, DiscoveryError> {
        self.inner.get(id)
    }
}
