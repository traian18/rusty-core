//! Bounded in-memory storage for published snapshots.
use std::{
    collections::VecDeque,
    sync::{Arc, RwLock},
};

use super::{
    contracts::RepositorySnapshotRepository,
    ids::SnapshotId,
    model::{DiscoveryError, RepositorySnapshot},
};

/// Keeps the newest `retention` snapshots and evicts older generations.
pub struct InMemoryRepositorySnapshotRepository {
    retention: usize,
    snapshots: RwLock<VecDeque<Arc<RepositorySnapshot>>>,
}

impl InMemoryRepositorySnapshotRepository {
    pub fn new(retention: usize) -> Result<Self, DiscoveryError> {
        if retention == 0 {
            return Err(DiscoveryError::Failed(
                "snapshot retention must be positive".into(),
            ));
        }
        Ok(Self {
            retention,
            snapshots: RwLock::new(VecDeque::new()),
        })
    }
}

impl RepositorySnapshotRepository for InMemoryRepositorySnapshotRepository {
    fn current(&self) -> Option<Arc<RepositorySnapshot>> {
        self.snapshots
            .read()
            .ok()
            .and_then(|snapshots| snapshots.back().cloned())
    }

    fn publish(&self, snapshot: RepositorySnapshot) -> Result<(), DiscoveryError> {
        let mut snapshots = self
            .snapshots
            .write()
            .map_err(|_| DiscoveryError::Failed("snapshot lock poisoned".into()))?;
        let snapshot = Arc::new(snapshot);
        snapshots.retain(|item| item.id != snapshot.id);
        snapshots.push_back(snapshot);
        while snapshots.len() > self.retention {
            snapshots.pop_front();
        }
        Ok(())
    }

    fn get(&self, id: &SnapshotId) -> Result<Arc<RepositorySnapshot>, DiscoveryError> {
        self.snapshots
            .read()
            .map_err(|_| DiscoveryError::Failed("snapshot lock poisoned".into()))?
            .iter()
            .find(|snapshot| snapshot.id == *id)
            .cloned()
            .ok_or_else(|| DiscoveryError::SnapshotNotFound(id.0.clone()))
    }
}
