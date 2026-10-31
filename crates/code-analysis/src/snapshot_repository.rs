use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, RwLock};

use crate::api::CallGraphError;
use crate::model::{CallSite, CallableSymbol, Diagnostic, RefreshStats, SnapshotId, SymbolId};

/// Lookup tables over one immutable generation.
#[derive(Clone, Default)]
pub(crate) struct GraphIndex {
    pub(crate) callable_by_id: BTreeMap<SymbolId, usize>,
    /// Call-site indices by owning callable.
    pub(crate) by_caller: BTreeMap<SymbolId, Vec<usize>>,
    /// Call-site indices by resolved target or possible candidate.
    pub(crate) by_target: BTreeMap<SymbolId, Vec<usize>>,
}

/// An immutable, fully built call-graph generation.
#[derive(Clone)]
pub struct GraphSnapshot {
    pub(crate) id: SnapshotId,
    pub(crate) callables: Vec<CallableSymbol>,
    pub(crate) calls: Vec<CallSite>,
    pub(crate) partial: bool,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) limits: Vec<String>,
    pub(crate) freshness: String,
    pub(crate) generated_at_unix: u64,
    pub(crate) stats: RefreshStats,
    pub(crate) index: GraphIndex,
}

pub(crate) struct SnapshotParts {
    pub id: SnapshotId,
    pub callables: Vec<CallableSymbol>,
    pub calls: Vec<CallSite>,
    pub partial: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub limits: Vec<String>,
    pub freshness: String,
    pub generated_at_unix: u64,
    pub stats: RefreshStats,
}

impl GraphSnapshot {
    pub(crate) fn new(parts: SnapshotParts) -> Self {
        let mut index = GraphIndex::default();
        for (position, callable) in parts.callables.iter().enumerate() {
            index.callable_by_id.insert(callable.id.clone(), position);
        }
        for (position, call) in parts.calls.iter().enumerate() {
            index
                .by_caller
                .entry(call.caller.clone())
                .or_default()
                .push(position);
            for target in call.target.iter().chain(call.candidates.iter()) {
                index
                    .by_target
                    .entry(target.clone())
                    .or_default()
                    .push(position);
            }
        }
        Self {
            id: parts.id,
            callables: parts.callables,
            calls: parts.calls,
            partial: parts.partial,
            diagnostics: parts.diagnostics,
            limits: parts.limits,
            freshness: parts.freshness,
            generated_at_unix: parts.generated_at_unix,
            stats: parts.stats,
            index,
        }
    }

    pub fn generated_at_unix(&self) -> u64 {
        self.generated_at_unix
    }
    pub fn stats(&self) -> &RefreshStats {
        &self.stats
    }
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
