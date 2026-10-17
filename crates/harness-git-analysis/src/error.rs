use thiserror::Error;

/// Why history could not be read or analysed.
#[derive(Debug, Error)]
pub enum GitAnalysisError {
    #[error("git analysis cancelled")]
    Cancelled,
    #[error("not a git repository: {0}")]
    NotARepository(String),
    #[error("invalid git analysis configuration: {0}")]
    InvalidConfig(String),
    #[error("git analysis failed: {0}")]
    Failed(String),
}

/// Why a query over a published snapshot failed.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum GitQueryError {
    #[error("git snapshot not found: {0}")]
    SnapshotNotFound(String),
    #[error("limit must be between 1 and {max}")]
    InvalidLimit { max: usize },
    #[error("cursor belongs to a different snapshot")]
    ForeignCursor,
    #[error("cursor does not match this query")]
    InvalidCursor,
    #[error("no history for file: {0}")]
    UnknownFile(String),
    #[error("invalid query: {0}")]
    InvalidQuery(String),
}
