use thiserror::Error;

use harness_workspace::repository_intelligence::SnapshotId;

/// Why an analysis could not run.
#[derive(Debug, Error)]
pub enum FindingsError {
    #[error("findings analysis cancelled")]
    Cancelled,
    #[error("invalid findings configuration: {0}")]
    InvalidConfig(String),
    #[error("repository snapshot not found: {0:?}")]
    RepositorySnapshotNotFound(SnapshotId),
    #[error("no repository snapshot is published yet")]
    NoRepositorySnapshot,
    #[error("git snapshot not found: {0}")]
    GitSnapshotNotFound(String),
    #[error("findings analysis failed: {0}")]
    Failed(String),
}

/// Why a query over a published finding set failed.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum FindingsQueryError {
    #[error("finding set not found: {0}")]
    SetNotFound(String),
    #[error("finding not found: {0}")]
    FindingNotFound(String),
    #[error("limit must be between 1 and {max}")]
    InvalidLimit { max: usize },
    #[error("cursor belongs to a different finding set")]
    ForeignCursor,
    #[error("cursor does not match this query")]
    InvalidCursor,
}
