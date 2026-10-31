//! Bounded in-memory storage for Git snapshots.
use std::{
    collections::VecDeque,
    sync::{Arc, RwLock},
};

use crate::{
    contracts::GitSnapshotRepository,
    error::{GitAnalysisError, GitQueryError},
    snapshot::{GitSnapshot, GitSnapshotId},
};

/// Keeps the newest `retention` snapshots.
pub struct InMemoryGitSnapshotRepository {
    retention: usize,
    snapshots: RwLock<VecDeque<Arc<GitSnapshot>>>,
}

impl InMemoryGitSnapshotRepository {
    pub fn new(retention: usize) -> Result<Self, GitAnalysisError> {
        if retention == 0 {
            return Err(GitAnalysisError::InvalidConfig(
                "snapshot retention must be positive".into(),
            ));
        }
        Ok(Self {
            retention,
            snapshots: RwLock::new(VecDeque::new()),
        })
    }
}

impl GitSnapshotRepository for InMemoryGitSnapshotRepository {
    fn current(&self) -> Option<Arc<GitSnapshot>> {
        self.snapshots.read().ok()?.back().cloned()
    }

    fn publish(&self, snapshot: GitSnapshot) -> Result<(), GitAnalysisError> {
        let mut snapshots = self
            .snapshots
            .write()
            .map_err(|_| GitAnalysisError::Failed("snapshot lock poisoned".into()))?;
        snapshots.retain(|existing| existing.id != snapshot.id);
        snapshots.push_back(Arc::new(snapshot));
        while snapshots.len() > self.retention {
            snapshots.pop_front();
        }
        Ok(())
    }

    fn get(&self, id: &GitSnapshotId) -> Result<Arc<GitSnapshot>, GitQueryError> {
        self.snapshots
            .read()
            .ok()
            .and_then(|snapshots| snapshots.iter().find(|s| s.id == *id).cloned())
            .ok_or_else(|| GitQueryError::SnapshotNotFound(id.0.clone()))
    }
}
