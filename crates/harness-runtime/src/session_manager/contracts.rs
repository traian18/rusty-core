//! Trait contracts and interface definitions for session management.
//!
//! These behavioral traits separate session management into distinct, cohesive
//! responsibilities following the Interface Segregation Principle (ISP) and
//! Dependency Inversion Principle (DIP):
//!
//! - [`SessionRegistry`]: Querying and subscribing to active sessions.
//! - [`SessionLifecycle`]: Session creation, graceful shutdown, and bulk drain.
//! - [`SessionRestorer`]: Reconstruction and recovery of sessions from durable storage.
//! - [`SessionService`]: Composite trait combining all session capabilities.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;

use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::SessionId;
use harness_protocol::tools::AgentToolset;

use crate::integration::IntegrationRegistry;
use crate::session_manager::errors::SessionManagerError;
use crate::session_runtime::SessionRuntime;
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

/// Contract for looking up, counting, and subscribing to active sessions.
#[async_trait]
pub trait SessionRegistry: Send + Sync {
    /// Retrieves a handle to an active session runtime by its identifier.
    ///
    /// Returns `Some(Arc<SessionRuntime>)` if the session is currently active,
    /// or `None` if it is not found.
    async fn session_handle(&self, id: SessionId) -> Option<Arc<SessionRuntime>>;

    /// Returns the number of sessions currently active in memory.
    async fn active_session_count(&self) -> usize;

    /// Returns the identifiers of all sessions currently active in memory.
    async fn active_session_ids(&self) -> Vec<SessionId>;

    /// Subscribes to the broadcast event stream of an active session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionManagerError::NotFound`] if the session is not active.
    async fn subscribe(
        &self,
        id: SessionId,
    ) -> Result<broadcast::Receiver<AgentEventEnvelope>, SessionManagerError>;
}

/// Contract for managing session creation, termination, and bulk draining.
#[async_trait]
pub trait SessionLifecycle: Send + Sync {
    /// Creates and starts a new session with bounded-wait scheduler admission.
    ///
    /// Waits up to the scheduler's configured admission timeout for a session permit.
    /// Spawns background supervision for the session's root agent task and creates
    /// an initial checkpoint if a session store is configured.
    ///
    /// # Errors
    ///
    /// Returns [`SessionManagerError::AtCapacity`] if no capacity permit is available.
    async fn create_session(
        &self,
        backend: Arc<dyn ExecutionBackend>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        root_toolset: AgentToolset,
    ) -> Result<Arc<SessionRuntime>, SessionManagerError>;

    /// Shuts down an active session and persists its final checkpoint.
    ///
    /// The session is removed from active tracking and its permit is released
    /// regardless of whether checkpointing succeeds.
    ///
    /// # Errors
    ///
    /// - [`SessionManagerError::NotFound`]: The session was not found in the active registry.
    /// - [`SessionManagerError::CloseCheckpointFailed`]: The final checkpoint could not be saved.
    async fn close_session(&self, id: SessionId) -> Result<(), SessionManagerError>;

    /// Concurrently closes all currently active sessions within a bounded grace period.
    ///
    /// Returns a list of session identifiers that either failed to checkpoint cleanly
    /// or exceeded the grace period.
    async fn close_all_sessions(&self, grace_period: Duration) -> Vec<SessionId>;
}

/// Contract for reconstructing sessions from durable storage without replaying side effects.
#[async_trait]
pub trait SessionRestorer: Send + Sync {
    /// Restores a session from the persistent session store.
    ///
    /// The stored snapshot is migrated, trailing events are validated and replayed,
    /// host dependencies are resolved according to policy, and required backend
    /// integrations are instantiated.
    ///
    /// If the session is already active in memory, returns the existing active runtime.
    ///
    /// # Errors
    ///
    /// Returns a [`SessionManagerError`] if the store is unconfigured, the snapshot
    /// is missing or invalid, or backend creation fails.
    async fn restore_session(
        &self,
        id: SessionId,
        integrations: Arc<IntegrationRegistry>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
    ) -> Result<Arc<SessionRuntime>, SessionManagerError>;
}

/// Composite service trait combining registry, lifecycle, and restoration capabilities.
pub trait SessionService: SessionRegistry + SessionLifecycle + SessionRestorer + Send + Sync {}

// Blanket implementation for any type implementing all constituent traits.
impl<T> SessionService for T where T: SessionRegistry + SessionLifecycle + SessionRestorer + Send + Sync {}
