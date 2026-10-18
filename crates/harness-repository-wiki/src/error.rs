//! Errors for page requests. A path that exists but cannot be described is a page with a status,
//! never an error.
use harness_workspace::{
    dependency_groups::GroupQueryError, import_graph::ImportQueryError,
    repository_intelligence::DiscoveryError,
};

#[derive(Debug, Eq, PartialEq)]
pub enum WikiError {
    /// The id does not name a level the wiki has.
    InvalidId(String),
    /// No snapshot has been published, or the named one is gone.
    NoSnapshot,
    /// A query on a contract failed.
    Query(String),
}

impl WikiError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidId(_) => "invalid_request",
            Self::NoSnapshot => "snapshot_not_found",
            Self::Query(_) => "dependency_failure",
        }
    }
}

impl std::fmt::Display for WikiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidId(id) => write!(formatter, "not a page id: {id}"),
            Self::NoSnapshot => {
                formatter.write_str("no repository snapshot has been published yet")
            }
            Self::Query(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for WikiError {}

impl From<DiscoveryError> for WikiError {
    fn from(error: DiscoveryError) -> Self {
        match error {
            DiscoveryError::SnapshotNotFound(_) => Self::NoSnapshot,
            other => Self::Query(other.to_string()),
        }
    }
}

impl From<GroupQueryError> for WikiError {
    fn from(error: GroupQueryError) -> Self {
        Self::Query(error.to_string())
    }
}

impl From<ImportQueryError> for WikiError {
    fn from(error: ImportQueryError) -> Self {
        Self::Query(error.to_string())
    }
}
