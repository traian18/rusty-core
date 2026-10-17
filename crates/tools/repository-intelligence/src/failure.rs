//! Stable error codes for tool results, mapped one-to-one from the contracts' typed errors.
use harness_findings::{FindingsError, FindingsQueryError};
use harness_git_analysis::{GitAnalysisError, GitQueryError};
use harness_workspace::{
    dependency_groups::GroupQueryError, framework_relations::FrameworkQueryError,
    import_graph::ImportQueryError, repository_intelligence::DiscoveryError,
};

/// A failed operation: a stable machine-readable code and a human-readable message.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Failure {
    pub code: &'static str,
    pub message: String,
}

impl Failure {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }

    pub fn cancelled() -> Self {
        Self::new("cancelled", "the operation was cancelled")
    }

    pub fn not_configured(message: impl Into<String>) -> Self {
        Self::new("not_configured", message)
    }
}

impl From<ImportQueryError> for Failure {
    fn from(error: ImportQueryError) -> Self {
        let code = match &error {
            ImportQueryError::SnapshotNotFound(_) => "snapshot_not_found",
            ImportQueryError::InvalidLimit { .. } => "limit_exceeded",
            ImportQueryError::ForeignCursor | ImportQueryError::InvalidCursor => "invalid_cursor",
            ImportQueryError::UnknownFile(_) => "unknown_file",
            ImportQueryError::InvalidQuery(_) => "invalid_request",
        };
        Self::new(code, error.to_string())
    }
}

impl From<GroupQueryError> for Failure {
    fn from(error: GroupQueryError) -> Self {
        let code = match &error {
            GroupQueryError::SnapshotNotFound(_) => "snapshot_not_found",
            GroupQueryError::NotConfigured => "not_configured",
            GroupQueryError::InvalidLimit { .. } => "limit_exceeded",
            GroupQueryError::ForeignCursor | GroupQueryError::InvalidCursor => "invalid_cursor",
            GroupQueryError::UnknownGroup(_) => "unknown_group",
            GroupQueryError::UnknownFile(_) => "unknown_file",
            GroupQueryError::InvalidQuery(_) => "invalid_request",
        };
        Self::new(code, error.to_string())
    }
}

impl From<FrameworkQueryError> for Failure {
    fn from(error: FrameworkQueryError) -> Self {
        let code = match &error {
            FrameworkQueryError::NotConfigured => "not_configured",
            FrameworkQueryError::SnapshotNotFound(_) => "snapshot_not_found",
            FrameworkQueryError::InvalidLimit { .. } => "limit_exceeded",
            FrameworkQueryError::ForeignCursor | FrameworkQueryError::InvalidCursor => {
                "invalid_cursor"
            }
        };
        Self::new(code, error.to_string())
    }
}

impl From<DiscoveryError> for Failure {
    fn from(error: DiscoveryError) -> Self {
        let code = match &error {
            DiscoveryError::Cancelled => "cancelled",
            DiscoveryError::SnapshotNotFound(_) => "snapshot_not_found",
            DiscoveryError::InvalidRelativePath(_) => "invalid_request",
            DiscoveryError::Failed(_) => "dependency_failure",
        };
        Self::new(code, error.to_string())
    }
}

impl From<GitQueryError> for Failure {
    fn from(error: GitQueryError) -> Self {
        let code = match &error {
            GitQueryError::SnapshotNotFound(_) => "snapshot_not_found",
            GitQueryError::InvalidLimit { .. } => "limit_exceeded",
            GitQueryError::ForeignCursor | GitQueryError::InvalidCursor => "invalid_cursor",
            GitQueryError::UnknownFile(_) => "unknown_file",
            GitQueryError::InvalidQuery(_) => "invalid_request",
        };
        Self::new(code, error.to_string())
    }
}

impl From<GitAnalysisError> for Failure {
    fn from(error: GitAnalysisError) -> Self {
        let code = match &error {
            GitAnalysisError::Cancelled => "cancelled",
            GitAnalysisError::InvalidConfig(_) => "invalid_request",
            GitAnalysisError::NotARepository(_) | GitAnalysisError::Failed(_) => {
                "dependency_failure"
            }
        };
        Self::new(code, error.to_string())
    }
}

impl From<FindingsError> for Failure {
    fn from(error: FindingsError) -> Self {
        let code = match &error {
            FindingsError::Cancelled => "cancelled",
            FindingsError::InvalidConfig(_) => "invalid_request",
            FindingsError::RepositorySnapshotNotFound(_)
            | FindingsError::NoRepositorySnapshot
            | FindingsError::GitSnapshotNotFound(_) => "snapshot_not_found",
            FindingsError::Failed(_) => "dependency_failure",
        };
        Self::new(code, error.to_string())
    }
}

impl From<FindingsQueryError> for Failure {
    fn from(error: FindingsQueryError) -> Self {
        let code = match &error {
            FindingsQueryError::SetNotFound(_) => "snapshot_not_found",
            FindingsQueryError::FindingNotFound(_) => "finding_not_found",
            FindingsQueryError::InvalidLimit { .. } => "limit_exceeded",
            FindingsQueryError::ForeignCursor | FindingsQueryError::InvalidCursor => {
                "invalid_cursor"
            }
        };
        Self::new(code, error.to_string())
    }
}
