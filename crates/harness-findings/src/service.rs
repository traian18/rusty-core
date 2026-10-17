//! The consumer API: analyse chosen snapshots, then query the published finding sets.
use std::{
    collections::VecDeque,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use harness_git_analysis::{GitAnalysis, GitSnapshotId};
use harness_workspace::{
    import_graph::ImportEdge,
    repository_intelligence::{RepositoryGraph, SnapshotId},
};

use crate::{
    analyzer::{AnalysisInput, FindingsAnalyzer},
    error::{FindingsError, FindingsQueryError},
    model::FindingId,
    set::{
        FindingEntry, FindingSet, FindingSetId, FindingsListQuery, FindingsPage, FindingsSummary,
    },
    source::SourceTextReader,
};

const DEFAULT_RETENTION: usize = 4;

/// Which snapshots to analyse; omitted ids mean "the current one".
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct AnalyzeRequest {
    pub repository_snapshot: Option<SnapshotId>,
    pub git_snapshot: Option<GitSnapshotId>,
    /// When false, Git-based rules are skipped even if a Git snapshot exists.
    pub use_git: bool,
}

/// A finding with its rationale and the evidence expanded into full import edges.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FindingExplanation {
    pub entry: FindingEntry,
    pub rule_description: Option<String>,
    pub import_sites: Vec<ImportEdge>,
    /// False when the repository snapshot has been evicted, so import sites could not be expanded.
    pub repository_snapshot_available: bool,
}

#[async_trait]
pub trait FindingsAnalysis: Send + Sync {
    async fn analyze(
        &self,
        request: AnalyzeRequest,
        cancel: CancellationToken,
    ) -> Result<Arc<FindingSet>, FindingsError>;
    fn current(&self) -> Option<Arc<FindingSet>>;
    fn get(&self, id: &FindingSetId) -> Result<Arc<FindingSet>, FindingsQueryError>;
    fn summary(&self, id: &FindingSetId) -> Result<FindingsSummary, FindingsQueryError>;
    fn list(&self, query: &FindingsListQuery) -> Result<FindingsPage, FindingsQueryError>;
    fn finding(
        &self,
        set: &FindingSetId,
        id: &FindingId,
    ) -> Result<FindingEntry, FindingsQueryError>;
    fn explain(
        &self,
        set: &FindingSetId,
        id: &FindingId,
    ) -> Result<FindingExplanation, FindingsQueryError>;
}

/// Bounded storage for recent finding sets; identical content has an identical id, so
/// republishing replaces rather than duplicates.
struct SetStore {
    retention: usize,
    sets: RwLock<VecDeque<Arc<FindingSet>>>,
}

impl SetStore {
    fn publish(&self, set: FindingSet) -> Arc<FindingSet> {
        let set = Arc::new(set);
        if let Ok(mut sets) = self.sets.write() {
            sets.retain(|existing| existing.id != set.id);
            sets.push_back(set.clone());
            while sets.len() > self.retention {
                sets.pop_front();
            }
        }
        set
    }

    fn current(&self) -> Option<Arc<FindingSet>> {
        self.sets.read().ok()?.back().cloned()
    }

    fn get(&self, id: &FindingSetId) -> Result<Arc<FindingSet>, FindingsQueryError> {
        self.sets
            .read()
            .ok()
            .and_then(|sets| sets.iter().find(|set| set.id == *id).cloned())
            .ok_or_else(|| FindingsQueryError::SetNotFound(id.0.clone()))
    }
}

pub struct FindingsService {
    graph: Arc<dyn RepositoryGraph>,
    git: Option<Arc<dyn GitAnalysis>>,
    analyzer: Arc<dyn FindingsAnalyzer>,
    reader: Arc<dyn SourceTextReader>,
    store: SetStore,
}

impl FindingsService {
    pub fn new(
        graph: Arc<dyn RepositoryGraph>,
        git: Option<Arc<dyn GitAnalysis>>,
        analyzer: Arc<dyn FindingsAnalyzer>,
        reader: Arc<dyn SourceTextReader>,
    ) -> Self {
        Self {
            graph,
            git,
            analyzer,
            reader,
            store: SetStore {
                retention: DEFAULT_RETENTION,
                sets: RwLock::new(VecDeque::new()),
            },
        }
    }
}

#[async_trait]
impl FindingsAnalysis for FindingsService {
    async fn analyze(
        &self,
        request: AnalyzeRequest,
        cancel: CancellationToken,
    ) -> Result<Arc<FindingSet>, FindingsError> {
        if cancel.is_cancelled() {
            return Err(FindingsError::Cancelled);
        }
        let repository = match &request.repository_snapshot {
            Some(id) => self
                .graph
                .get(id)
                .map_err(|_| FindingsError::RepositorySnapshotNotFound(id.clone()))?,
            None => self
                .graph
                .current()
                .ok_or(FindingsError::NoRepositorySnapshot)?,
        };
        let git = match (&self.git, request.use_git, &request.git_snapshot) {
            (Some(git), true, Some(id)) => Some(
                git.get(id)
                    .map_err(|_| FindingsError::GitSnapshotNotFound(id.0.clone()))?,
            ),
            (Some(git), true, None) => git.current(),
            _ => None,
        };
        let (analyzer, reader, token) =
            (self.analyzer.clone(), self.reader.clone(), cancel.clone());
        let set = tokio::task::spawn_blocking(move || {
            analyzer.analyze(
                &AnalysisInput {
                    repository: &repository,
                    git: git.as_deref(),
                    reader: reader.as_ref(),
                },
                &token,
            )
        })
        .await
        .map_err(|error| FindingsError::Failed(error.to_string()))??;
        if cancel.is_cancelled() {
            return Err(FindingsError::Cancelled);
        }
        tracing::info!(
            target: "findings",
            findings = set.summary.total,
            suppressed = set.summary.suppressed,
            skipped_rules = set.summary.skipped_rules.len(),
            "findings analysis finished"
        );
        Ok(self.store.publish(set))
    }

    fn current(&self) -> Option<Arc<FindingSet>> {
        self.store.current()
    }

    fn get(&self, id: &FindingSetId) -> Result<Arc<FindingSet>, FindingsQueryError> {
        self.store.get(id)
    }

    fn summary(&self, id: &FindingSetId) -> Result<FindingsSummary, FindingsQueryError> {
        Ok(self.store.get(id)?.summary.clone())
    }

    fn list(&self, query: &FindingsListQuery) -> Result<FindingsPage, FindingsQueryError> {
        self.store.get(&query.set)?.list(query)
    }

    fn finding(
        &self,
        set: &FindingSetId,
        id: &FindingId,
    ) -> Result<FindingEntry, FindingsQueryError> {
        self.store.get(set)?.find(id)
    }

    fn explain(
        &self,
        set: &FindingSetId,
        id: &FindingId,
    ) -> Result<FindingExplanation, FindingsQueryError> {
        let set = self.store.get(set)?;
        let entry = set.find(id)?;
        let snapshot = self.graph.get(&set.repository_snapshot).ok();
        let import_sites = snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .imports
                    .edges
                    .iter()
                    .filter(|edge| entry.finding.evidence.import_sites.contains(&edge.fact.id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        Ok(FindingExplanation {
            rule_description: self
                .analyzer
                .describe_rule(&entry.finding.rule.id)
                .map(str::to_owned),
            entry,
            import_sites,
            repository_snapshot_available: snapshot.is_some(),
        })
    }
}
