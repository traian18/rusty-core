use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

use crate::api::CallGraphError;
use crate::model::{CallSite, CallableSymbol, Diagnostic, SnapshotId};

#[derive(Clone)]
pub struct GraphSnapshot {
    pub(crate) id: SnapshotId,
    pub(crate) callables: Vec<CallableSymbol>,
    pub(crate) calls: Vec<CallSite>,
    pub(crate) partial: bool,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) limits: Vec<String>,
    pub(crate) freshness: String,
}

impl GraphSnapshot {
    pub fn id(&self) -> &SnapshotId {
        &self.id
    }
    pub fn callables(&self) -> &[CallableSymbol] {
        &self.callables
    }
    pub fn calls(&self) -> &[CallSite] {
        &self.calls
    }
    pub fn partial(&self) -> bool {
        self.partial
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub fn limits(&self) -> &[String] {
        &self.limits
    }
    pub fn freshness(&self) -> &str {
        &self.freshness
    }
}

pub trait SnapshotRepository: Send + Sync {
    fn current(&self) -> Option<Arc<GraphSnapshot>>;
    fn publish(&self, snapshot: GraphSnapshot) -> Result<(), CallGraphError>;
    fn get(&self, id: &SnapshotId) -> Result<Arc<GraphSnapshot>, CallGraphError>;
}

pub struct InMemorySnapshotRepository {
    retention: usize,
    snapshots: RwLock<VecDeque<Arc<GraphSnapshot>>>,
}

impl InMemorySnapshotRepository {
    pub fn new(retention: usize) -> Result<Self, CallGraphError> {
        if retention == 0 {
            return Err(CallGraphError::InvalidRequest(
                "snapshot retention must be positive".into(),
            ));
        }
        Ok(Self {
            retention,
            snapshots: RwLock::new(VecDeque::new()),
        })
    }
}

impl SnapshotRepository for InMemorySnapshotRepository {
    fn current(&self) -> Option<Arc<GraphSnapshot>> {
        self.snapshots
            .read()
            .ok()
            .and_then(|items| items.back().cloned())
    }

    fn publish(&self, snapshot: GraphSnapshot) -> Result<(), CallGraphError> {
        let mut items = self
            .snapshots
            .write()
            .map_err(|_| CallGraphError::DependencyFailure("snapshot lock poisoned".into()))?;
        let snapshot = Arc::new(snapshot);
        items.retain(|item| item.id != snapshot.id);
        items.push_back(snapshot);
        while items.len() > self.retention {
            items.pop_front();
        }
        Ok(())
    }

    fn get(&self, id: &SnapshotId) -> Result<Arc<GraphSnapshot>, CallGraphError> {
        self.snapshots
            .read()
            .map_err(|_| CallGraphError::DependencyFailure("snapshot lock poisoned".into()))?
            .iter()
            .find(|item| item.id == *id)
            .cloned()
            .ok_or_else(|| CallGraphError::SnapshotNotFound(id.clone()))
    }
}
