//! Reading the history window, reusing retained commits when only new commits arrived.
use tokio_util::sync::CancellationToken;

use crate::{
    config::GitAnalysisConfig,
    contracts::{CommitBatch, GitHistorySource, HeadInfo, HistoryRequest, WalkStop},
    error::GitAnalysisError,
    model::{CommitFact, CommitId},
};

const SECONDS_PER_DAY: i64 = 86_400;

/// The commits behind the last snapshot, kept so the next refresh can extend them.
#[derive(Clone, Debug)]
pub struct RetainedHistory {
    pub head: CommitId,
    pub config_digest: String,
    pub batch: CommitBatch,
}

/// Where the commits for a refresh came from.
pub enum HistoryOrigin {
    /// `HEAD` has not moved since the retained history was read.
    Unchanged,
    /// Only commits after the retained head were read.
    Extended,
    Full,
}

pub struct HistoryRead {
    pub head: HeadInfo,
    pub batch: CommitBatch,
    pub origin: HistoryOrigin,
}

/// Reads the configured window ending at `HEAD`.
pub fn read_window(
    source: &dyn GitHistorySource,
    config: &GitAnalysisConfig,
    retained: Option<&RetainedHistory>,
    cancel: &CancellationToken,
) -> Result<HistoryRead, GitAnalysisError> {
    let head = source.head()?;
    let cutoff = head.author_time - i64::from(config.max_age_days) * SECONDS_PER_DAY;
    let retained = retained.filter(|r| r.config_digest == config.digest());
    if let Some(previous) = retained {
        if previous.head == head.id {
            return Ok(HistoryRead {
                batch: previous.batch.clone(),
                head,
                origin: HistoryOrigin::Unchanged,
            });
        }
        if let Some(batch) = extend(source, config, previous, &head, cutoff, cancel)? {
            return Ok(HistoryRead {
                head,
                batch,
                origin: HistoryOrigin::Extended,
            });
        }
    }
    let request = request_for(config, cutoff, None);
    let batch = source.commits(&request, cancel)?;
    Ok(HistoryRead {
        head,
        batch,
        origin: HistoryOrigin::Full,
    })
}

fn request_for(
    config: &GitAnalysisConfig,
    cutoff: i64,
    stop_at: Option<CommitId>,
) -> HistoryRequest {
    HistoryRequest {
        stop_at,
        max_commits: config.max_commits,
        max_walk: config.max_walk,
        cutoff_time: Some(cutoff),
        max_line_stat_files: config.max_line_stat_files,
    }
}

/// Prepends new commits to the retained ones when that provably equals a full read:
/// the old head is an ancestor, the new range is a plain chain without merges, and the
/// old walk was not cut by the walk cap.
fn extend(
    source: &dyn GitHistorySource,
    config: &GitAnalysisConfig,
    previous: &RetainedHistory,
    head: &HeadInfo,
    cutoff: i64,
    cancel: &CancellationToken,
) -> Result<Option<CommitBatch>, GitAnalysisError> {
    if previous.batch.stop == WalkStop::WalkCap || !source.is_ancestor(&previous.head, &head.id)? {
        return Ok(None);
    }
    let request = request_for(config, cutoff, Some(previous.head.clone()));
    let newer = source.commits(&request, cancel)?;
    if newer.stop != WalkStop::Exhausted || newer.commits.iter().any(CommitFact::is_merge) {
        return Ok(None);
    }
    let mut commits = newer.commits;
    commits.extend(previous.batch.commits.iter().cloned());
    Ok(Some(apply_window(
        commits,
        previous.batch.stop,
        config.max_commits,
        cutoff,
    )))
}

/// Trims newest-first `commits` to the window, deciding the stop reason in the same order a
/// fresh walk would: the commit limit is checked before the age of the next commit.
fn apply_window(
    mut commits: Vec<CommitFact>,
    earlier_stop: WalkStop,
    max_commits: usize,
    cutoff: i64,
) -> CommitBatch {
    let within_limit = commits.len().min(max_commits);
    let stop = if let Some(position) = commits[..within_limit]
        .iter()
        .position(|commit| commit.author_time < cutoff)
    {
        commits.truncate(position);
        WalkStop::AgeLimit
    } else if commits.len() > max_commits {
        commits.truncate(max_commits);
        WalkStop::CommitLimit
    } else {
        earlier_stop
    };
    let examined = commits.len() + usize::from(stop != WalkStop::Exhausted);
    CommitBatch {
        commits,
        examined,
        stop,
    }
}
