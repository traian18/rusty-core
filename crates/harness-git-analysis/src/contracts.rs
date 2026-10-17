//! Replaceable boundaries of Git analysis.
use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use harness_workspace::repository_intelligence::{
    RepositorySnapshot, RootRelativePath, SnapshotId,
};
use tokio_util::sync::CancellationToken;

use crate::{
    error::{GitAnalysisError, GitQueryError},
    model::{CommitFact, CommitId},
    queries::{
        ChurnPage, ChurnQuery, CoChangePage, CoChangeQuery, ContributorPage, ContributorQuery,
    },
    snapshot::{GitCoverage, GitSnapshot, GitSnapshotId},
};

/// The state of `HEAD` when analysis starts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeadInfo {
    pub id: CommitId,
    pub author_time: i64,
    pub shallow: bool,
    /// Tracked files differ from `HEAD`; those changes are not analysed.
    pub working_tree_dirty: bool,
}

/// How much history to read, newest first from `HEAD`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryRequest {
    /// Read only commits not reachable from this one (for incremental refresh).
    pub stop_at: Option<CommitId>,
    pub max_commits: usize,
    pub max_walk: usize,
    /// Commits authored before this time are left out.
    pub cutoff_time: Option<i64>,
    /// Commits changing more files than this get no line statistics.
    pub max_line_stat_files: usize,
}

/// Why a history walk ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WalkStop {
    /// All reachable history was read.
    Exhausted,
    CommitLimit,
    AgeLimit,
    WalkCap,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitBatch {
    /// Newest first.
    pub commits: Vec<CommitFact>,
    /// Commits looked at, including those left out.
    pub examined: usize,
    pub stop: WalkStop,
}

/// Supplies normalized commits. The production implementation reads a local repository.
pub trait GitHistorySource: Send + Sync {
    fn head(&self) -> Result<HeadInfo, GitAnalysisError>;
    fn commits(
        &self,
        request: &HistoryRequest,
        cancel: &CancellationToken,
    ) -> Result<CommitBatch, GitAnalysisError>;
    /// True when `ancestor` equals `descendant` or is reachable from it.
    fn is_ancestor(
        &self,
        ancestor: &CommitId,
        descendant: &CommitId,
    ) -> Result<bool, GitAnalysisError>;
    /// The subset of `paths` that exist in the `HEAD` tree.
    fn present_at_head(
        &self,
        paths: &[RootRelativePath],
    ) -> Result<BTreeSet<RootRelativePath>, GitAnalysisError>;
}

/// Bounded storage for published Git snapshots.
pub trait GitSnapshotRepository: Send + Sync {
    fn current(&self) -> Option<Arc<GitSnapshot>>;
    fn publish(&self, snapshot: GitSnapshot) -> Result<(), GitAnalysisError>;
    fn get(&self, id: &GitSnapshotId) -> Result<Arc<GitSnapshot>, GitQueryError>;
}

/// The part of a repository snapshot that Git facts are joined to.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RepositoryCorrelation {
    pub snapshot: Option<SnapshotId>,
    /// Files the repository snapshot inventoried.
    pub known_files: BTreeSet<RootRelativePath>,
}

impl RepositoryCorrelation {
    pub fn from_snapshot(snapshot: &RepositorySnapshot) -> Self {
        let graph = &snapshot.graph;
        let known_files = graph
            .source_files
            .iter()
            .chain(&graph.config_files)
            .chain(graph.packages.iter().map(|package| &package.manifest))
            .cloned()
            .collect();
        Self {
            snapshot: Some(snapshot.id.clone()),
            known_files,
        }
    }
}

/// Consumer contract: historical correlation, never causation.
#[async_trait]
pub trait GitAnalysis: Send + Sync {
    /// Reads history and publishes a snapshot; returns the current one when nothing changed.
    async fn refresh(
        &self,
        correlation: RepositoryCorrelation,
        cancel: CancellationToken,
    ) -> Result<Arc<GitSnapshot>, GitAnalysisError>;
    fn current(&self) -> Option<Arc<GitSnapshot>>;
    fn get(&self, id: &GitSnapshotId) -> Result<Arc<GitSnapshot>, GitQueryError>;
    fn churn(&self, query: &ChurnQuery) -> Result<ChurnPage, GitQueryError>;
    fn co_changed(&self, query: &CoChangeQuery) -> Result<CoChangePage, GitQueryError>;
    fn contributors(&self, query: &ContributorQuery) -> Result<ContributorPage, GitQueryError>;
    fn coverage(&self, snapshot: &GitSnapshotId) -> Result<GitCoverage, GitQueryError>;
}
