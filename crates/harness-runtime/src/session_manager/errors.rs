//! Error types for session management.

use harness_protocol::ids::SessionId;
use harness_session_store::{ReplayError, RestoreError, SnapshotVersionError, StoreError};

/// Domain errors encountered during session lifecycle operations and restoration.
#[derive(Debug, thiserror::Error)]
pub enum SessionManagerError {
    /// The requested session does not exist in the active session registry.
    #[error("session {0} not found")]
    NotFound(SessionId),

    /// Attempted to restore a session when no backing session store was configured.
    #[error("session restore requires a configured session store")]
    RestoreNotSupported,

    /// An error occurred in the underlying durable session store.
    #[error("session store error: {0}")]
    Store(#[from] StoreError),

    /// The stored session record contains no valid snapshot to restore from.
    #[error("session {0} has no stored snapshot to restore from")]
    NoSnapshot(SessionId),

    /// Replay validation of trailing durable events failed.
    #[error("stored session failed replay validation: {0}")]
    RestoreValidation(#[from] ReplayError),

    /// The stored snapshot version cannot be migrated to the current schema.
    #[error("stored snapshot is not migratable: {0}")]
    SnapshotMigration(#[from] SnapshotVersionError),

    /// Host dependency resolution rejected the restoration request according to policy.
    #[error("restore rejected: {0}")]
    RestoreRejected(#[from] RestoreError),

    /// Failed to re-create an execution backend from the integration registry during restore.
    #[error("failed to re-create backend for integration {integration}: {message}")]
    BackendCreation {
        /// Name or identifier of the integration that failed creation.
        integration: String,
        /// Detailed failure message.
        message: String,
    },

    /// A session was removed from the active registry, but its final checkpoint failed to persist.
    #[error("session {session} was removed but its final checkpoint failed to persist: {error}")]
    CloseCheckpointFailed {
        /// The identifier of the closed session.
        session: SessionId,
        /// Description of the checkpoint persistence failure.
        error: String,
    },

    /// No session-permit slot became available within the scheduler's configured admission timeout.
    ///
    /// This provides a typed, fail-fast rejection instead of blocking the caller indefinitely at capacity.
    #[error("session admission rejected: {0}")]
    AtCapacity(#[from] crate::scheduler::CapacityError),
}
