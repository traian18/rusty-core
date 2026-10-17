//! The production [`GitAnalysis`]: reads history, aggregates it, and publishes snapshots.
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{
    aggregate::{aggregate, Aggregation},
    config::GitAnalysisConfig,
    contracts::{
        GitAnalysis, GitHistorySource, GitSnapshotRepository, HeadInfo, RepositoryCorrelation,
    },
    error::{GitAnalysisError, GitQueryError},
    history::{read_window, HistoryOrigin, HistoryRead, RetainedHistory},
    queries::{
        ChurnPage, ChurnQuery, CoChangePage, CoChangeQuery, ContributorPage, ContributorQuery,
    },
    snapshot::{GitCoverage, GitIndex, GitSnapshot, GitSnapshotId, INTERPRETATION},
    source::Libgit2HistorySource,
    store::InMemoryGitSnapshotRepository,
};

const DEFAULT_RETENTION: usize = 4;
const SAMPLE_SIZE: usize = 50;

/// Composes a [`GitAnalysisService`] from replaceable parts.
pub struct GitAnalysisBuilder {
    source: Arc<dyn GitHistorySource>,
    config: GitAnalysisConfig,
    snapshots: Option<Arc<dyn GitSnapshotRepository>>,
}

impl GitAnalysisBuilder {
    pub fn new(source: Arc<dyn GitHistorySource>) -> Self {
        Self {
            source,
            config: GitAnalysisConfig::default(),
            snapshots: None,
        }
    }

    /// Reads the local repository that contains `root` with libgit2.
    pub fn for_repository(root: impl Into<PathBuf>) -> Self {
        Self::new(Arc::new(Libgit2HistorySource::new(root)))
    }

    pub fn config(mut self, config: GitAnalysisConfig) -> Self {
        self.config = config;
        self
    }

    pub fn snapshot_repository(mut self, snapshots: Arc<dyn GitSnapshotRepository>) -> Self {
        self.snapshots = Some(snapshots);
        self
    }

    pub fn build(self) -> Result<GitAnalysisService, GitAnalysisError> {
        self.config.validate()?;
        let snapshots = match self.snapshots {
            Some(snapshots) => snapshots,
            None => Arc::new(InMemoryGitSnapshotRepository::new(DEFAULT_RETENTION)?),
        };
        Ok(GitAnalysisService {
            source: self.source,
            config: self.config,
            snapshots,
            retained: Mutex::new(None),
            refresh_lock: tokio::sync::Mutex::new(()),
        })
    }
}

pub struct GitAnalysisService {
    source: Arc<dyn GitHistorySource>,
    config: GitAnalysisConfig,
    snapshots: Arc<dyn GitSnapshotRepository>,
    retained: Mutex<Option<RetainedHistory>>,
    refresh_lock: tokio::sync::Mutex<()>,
}

/// What a blocking refresh produced.
enum Refreshed {
    /// The current snapshot already describes this `HEAD`, configuration and correlation.
    Unchanged,
    Built(Box<(GitSnapshot, RetainedHistory)>),
}

impl GitAnalysisService {
    fn retained(&self) -> Option<RetainedHistory> {
        self.retained.lock().ok().and_then(|r| r.clone())
    }

    fn snapshot(&self, id: &GitSnapshotId) -> Result<Arc<GitSnapshot>, GitQueryError> {
        self.snapshots.get(id)
    }
}

#[async_trait]
impl GitAnalysis for GitAnalysisService {
    async fn refresh(
        &self,
        correlation: RepositoryCorrelation,
        cancel: CancellationToken,
    ) -> Result<Arc<GitSnapshot>, GitAnalysisError> {
        if cancel.is_cancelled() {
            return Err(GitAnalysisError::Cancelled);
        }
        let _guard = cancel
            .run_until_cancelled(self.refresh_lock.lock())
            .await
            .ok_or(GitAnalysisError::Cancelled)?;
        let (source, config) = (self.source.clone(), self.config.clone());
        let (retained, current, token) =
            (self.retained(), self.snapshots.current(), cancel.clone());
        let started = Instant::now();
        let refreshed = tokio::task::spawn_blocking(move || {
            build_snapshot(&*source, &config, retained, current, &correlation, &token)
        })
        .await
        .map_err(|error| GitAnalysisError::Failed(error.to_string()))??;
        if cancel.is_cancelled() {
            return Err(GitAnalysisError::Cancelled);
        }
        match refreshed {
            Refreshed::Unchanged => self
                .snapshots
                .current()
                .ok_or_else(|| GitAnalysisError::Failed("current snapshot disappeared".into())),
            Refreshed::Built(built) => {
                let (snapshot, history) = *built;
                tracing::info!(
                    target: "git_analysis",
                    files = snapshot.index.churn.len(),
                    co_changes = snapshot.index.co_changes.len(),
                    commits_used = snapshot.coverage.commits_used,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "git analysis refreshed"
                );
                let id = snapshot.id.clone();
                self.snapshots.publish(snapshot)?;
                if let Ok(mut retained) = self.retained.lock() {
                    *retained = Some(history);
                }
                self.snapshot(&id)
                    .map_err(|error| GitAnalysisError::Failed(error.to_string()))
            }
        }
    }

    fn current(&self) -> Option<Arc<GitSnapshot>> {
        self.snapshots.current()
    }

    fn get(&self, id: &GitSnapshotId) -> Result<Arc<GitSnapshot>, GitQueryError> {
        self.snapshot(id)
    }

    fn churn(&self, query: &ChurnQuery) -> Result<ChurnPage, GitQueryError> {
        self.snapshot(&query.snapshot)?.churn_page(query)
    }

    fn co_changed(&self, query: &CoChangeQuery) -> Result<CoChangePage, GitQueryError> {
        self.snapshot(&query.snapshot)?.co_change_page(query)
    }

    fn contributors(&self, query: &ContributorQuery) -> Result<ContributorPage, GitQueryError> {
        self.snapshot(&query.snapshot)?.contributor_page(query)
    }

    fn coverage(&self, snapshot: &GitSnapshotId) -> Result<GitCoverage, GitQueryError> {
        Ok(self.snapshot(snapshot)?.coverage.clone())
    }
}

/// Reads the window, aggregates it, and joins it to the repository correlation.
fn build_snapshot(
    source: &dyn GitHistorySource,
    config: &GitAnalysisConfig,
    retained: Option<RetainedHistory>,
    current: Option<Arc<GitSnapshot>>,
    correlation: &RepositoryCorrelation,
    cancel: &CancellationToken,
) -> Result<Refreshed, GitAnalysisError> {
    let HistoryRead {
        head,
        batch,
        origin,
    } = read_window(source, config, retained.as_ref(), cancel)?;
    if cancel.is_cancelled() {
        return Err(GitAnalysisError::Cancelled);
    }
    if let (HistoryOrigin::Unchanged, Some(current)) = (&origin, &current) {
        if still_describes(current, &head, config, correlation) {
            return Ok(Refreshed::Unchanged);
        }
    }
    let Aggregation { index, counts } = aggregate(&batch.commits, config, |paths| {
        source.present_at_head(paths)
    })?;
    let coverage = coverage_of(&head, &batch, &counts, &index, correlation);
    let snapshot_id = GitSnapshot::compute_id(config, &correlation.snapshot, &coverage, &index);
    let snapshot = GitSnapshot {
        id: snapshot_id,
        head: head.id.clone(),
        repository_snapshot: correlation.snapshot.clone(),
        config: config.clone(),
        coverage,
        index,
    };
    let history = RetainedHistory {
        head: head.id,
        config_digest: config.digest(),
        batch,
    };
    Ok(Refreshed::Built(Box::new((snapshot, history))))
}

fn still_describes(
    snapshot: &GitSnapshot,
    head: &HeadInfo,
    config: &GitAnalysisConfig,
    correlation: &RepositoryCorrelation,
) -> bool {
    snapshot.head == head.id
        && snapshot.config == *config
        && snapshot.repository_snapshot == correlation.snapshot
        && snapshot.coverage.working_tree_dirty == head.working_tree_dirty
        && snapshot.coverage.shallow == head.shallow
}

fn coverage_of(
    head: &HeadInfo,
    batch: &crate::contracts::CommitBatch,
    counts: &crate::aggregate::AggregationCounts,
    index: &GitIndex,
    correlation: &RepositoryCorrelation,
) -> GitCoverage {
    let times = batch.commits.iter().map(|c| c.author_time);
    let (unknown, sample, without_history) = match correlation.snapshot {
        Some(_) => join_with_repository(index, correlation),
        None => (0, Vec::new(), 0),
    };
    GitCoverage {
        head: head.id.clone(),
        commits_examined: batch.examined,
        commits_read: batch.commits.len(),
        commits_used: counts.commits_used,
        merges_skipped: counts.merges_skipped,
        bulk_commits_excluded: counts.bulk_commits_excluded,
        excluded_changes: counts.excluded_changes,
        changes_without_lines: counts.changes_without_lines,
        shallow: head.shallow,
        earliest_commit_time: times.clone().min(),
        latest_commit_time: times.max(),
        walk_stop: batch.stop,
        working_tree_dirty: head.working_tree_dirty,
        not_in_repository_snapshot: unknown,
        not_in_repository_snapshot_sample: sample,
        repository_files_without_history: without_history,
        interpretation: INTERPRETATION.to_owned(),
    }
}

/// Files with history the repository snapshot lacks (count, sample) and inventoried files without history.
fn join_with_repository(
    index: &GitIndex,
    correlation: &RepositoryCorrelation,
) -> (
    usize,
    Vec<harness_workspace::repository_intelligence::RootRelativePath>,
    usize,
) {
    let unknown = index
        .churn
        .iter()
        .filter(|churn| !churn.absent_at_head && !correlation.known_files.contains(&churn.file))
        .map(|churn| churn.file.clone())
        .collect::<Vec<_>>();
    let with_history = index
        .churn
        .iter()
        .map(|c| &c.file)
        .collect::<std::collections::BTreeSet<_>>();
    let without_history = correlation
        .known_files
        .iter()
        .filter(|file| !with_history.contains(file))
        .count();
    let count = unknown.len();
    (
        count,
        unknown.into_iter().take(SAMPLE_SIZE).collect(),
        without_history,
    )
}
