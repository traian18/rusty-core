use std::fmt;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::model::{
    CallGraphCapabilities, CallableResponse, CallsFromRequest, CallsResponse, CallsToRequest,
    FindCallablesRequest, RefreshRequest, RefreshResponse, SnapshotId, TraversalRequest,
    TraversalResponse,
};

/// Consumer-facing call-graph contract.
#[async_trait::async_trait]
pub trait CallGraph: Send + Sync {
    async fn refresh(
        &self,
        request: RefreshRequest,
        cancel: CancellationToken,
    ) -> Result<RefreshResponse, CallGraphError>;

    fn capabilities(&self) -> CallGraphCapabilities;

    async fn find_callables(
        &self,
        request: FindCallablesRequest,
        cancel: CancellationToken,
    ) -> Result<CallableResponse, CallGraphError>;

    async fn calls_from(
        &self,
        request: CallsFromRequest,
        cancel: CancellationToken,
    ) -> Result<CallsResponse, CallGraphError>;

    async fn calls_to(
        &self,
        request: CallsToRequest,
        cancel: CancellationToken,
    ) -> Result<CallsResponse, CallGraphError>;

    async fn traverse(
        &self,
        request: TraversalRequest,
        cancel: CancellationToken,
    ) -> Result<TraversalResponse, CallGraphError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CallGraphError {
    #[serde(rename = "invalid_request")]
    InvalidRequest(String),
    #[serde(rename = "snapshot_not_found")]
    SnapshotNotFound(SnapshotId),
    #[serde(rename = "cancelled")]
    Cancelled,
    #[serde(rename = "dependency_failure")]
    DependencyFailure(String),
    #[serde(rename = "limit_exceeded")]
    LimitExceeded(String),
}

impl fmt::Display for CallGraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message) => write!(f, "invalid request: {message}"),
            Self::SnapshotNotFound(id) => write!(f, "snapshot not found: {id}"),
            Self::Cancelled => f.write_str("call-graph operation cancelled"),
            Self::DependencyFailure(message) => write!(f, "dependency failure: {message}"),
            Self::LimitExceeded(message) => write!(f, "limit exceeded: {message}"),
        }
    }
}

impl std::error::Error for CallGraphError {}
