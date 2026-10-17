//! The immutable result of one analysis run.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use harness_workspace::repository_intelligence::{RootRelativePath, SnapshotId};

use crate::{
    config::GitAnalysisConfig,
    contracts::WalkStop,
    model::{CommitId, Contributor},
};

/// Stated with every result so no consumer reads more into it than it says.
pub const INTERPRETATION: &str = "Historical correlation within the analysed window. It shows what \
changed together and who changed it recently; it does not show causation, dependency, or code ownership.";

#[derive(Clone, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct GitSnapshotId(pub String);

/// Changes to one file at its `HEAD` path, summed over the window.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileChurn {
    pub file: RootRelativePath,
    pub commits: u32,
    pub lines_added: u64,
    pub lines_removed: u64,
    /// Changes without line counts (binary files or skipped statistics).
    pub changes_without_lines: u32,
    pub contributors: u32,
    pub first_change: i64,
    pub last_change: i64,
    /// The file no longer exists at `HEAD`.
    pub absent_at_head: bool,
}

/// `file` and `partner` were changed in the same commits. This is correlation, not a dependency.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CoChange {
    pub file: RootRelativePath,
    pub partner: RootRelativePath,
    /// Commits that changed both.
    pub support: u32,
    /// Commits that changed `file`.
    pub file_commits: u32,
    /// `support / file_commits` in thousandths.
    pub confidence_per_mille: u32,
    /// Newest commits that changed both, bounded.
    pub example_commits: Vec<CommitId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContributorShare {
    pub contributor: Contributor,
    pub commits: u32,
    pub lines_changed: u64,
    pub commit_share_per_mille: u32,
    pub line_share_per_mille: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileContributors {
    pub file: RootRelativePath,
    /// Most commits first.
    pub entries: Vec<ContributorShare>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct GitIndex {
    /// Sorted by file.
    pub churn: Vec<FileChurn>,
    /// Sorted by file, strongest partner first within a file.
    pub co_changes: Vec<CoChange>,
    /// Sorted by file.
    pub contributors: Vec<FileContributors>,
}

/// What the analysis covered and what it left out.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GitCoverage {
    pub head: CommitId,
    pub commits_examined: usize,
    pub commits_read: usize,
    /// Commits with at least one analysed file change.
    pub commits_used: usize,
    pub merges_skipped: usize,
    /// Excluded from co-change for touching too many files; still counted in churn.
    pub bulk_commits_excluded: usize,
    pub excluded_changes: usize,
    pub changes_without_lines: usize,
    pub shallow: bool,
    pub earliest_commit_time: Option<i64>,
    pub latest_commit_time: Option<i64>,
    pub walk_stop: WalkStop,
    /// Tracked files differ from `HEAD`; those changes are not analysed.
    pub working_tree_dirty: bool,
    /// Files with history that the repository snapshot did not inventory.
    pub not_in_repository_snapshot: usize,
    pub not_in_repository_snapshot_sample: Vec<RootRelativePath>,
    /// Inventoried files with no change in the window.
    pub repository_files_without_history: usize,
    pub interpretation: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GitSnapshot {
    pub id: GitSnapshotId,
    pub head: CommitId,
    /// The repository snapshot the facts were joined to, if any.
    pub repository_snapshot: Option<SnapshotId>,
    pub config: GitAnalysisConfig,
    pub coverage: GitCoverage,
    pub index: GitIndex,
}

impl GitSnapshot {
    /// Deterministic over the head commit, configuration, correlation, coverage and index.
    pub fn compute_id(
        config: &GitAnalysisConfig,
        repository_snapshot: &Option<SnapshotId>,
        coverage: &GitCoverage,
        index: &GitIndex,
    ) -> GitSnapshotId {
        let bytes = serde_json::to_vec(&(
            "git-analysis-v1",
            config.digest(),
            repository_snapshot,
            coverage,
            index,
        ))
        .unwrap_or_default();
        GitSnapshotId(format!("{:x}", Sha256::digest(&bytes)))
    }
}
