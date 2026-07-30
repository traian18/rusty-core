//! Session lifecycle management for the harness runtime.
//!
//! SessionManager owns all active agent sessions and provides thread-safe,
//! asynchronous access to create, close, query, subscribe to, and eventually
//! restore them. Each session has a globally unique SessionId and an
//! Arc<SessionRuntime>.
//!
//! # Concurrency
//!
//! Session runtimes live in a tokio RwLock<HashMap<_, _>>, so lookups and
//! snapshots only need a read lock. A shared Scheduler supplies the global
//! concurrency ceilings. An owned session permit is retained until its session
//! is closed, limiting the number of active sessions.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{broadcast, OwnedSemaphorePermit, RwLock};

use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::SessionId;
use harness_protocol::tools::AgentToolset;

use crate::scheduler::Scheduler;
use crate::session_runtime::SessionRuntime;
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

/// Errors that can occur during session management operations.
#[derive(Debug, thiserror::Error)]
pub enum SessionManagerError {
    /// The requested session ID does not correspond to any active session.
    #[error("session {0} not found")]
    NotFound(SessionId),

    /// Session persistence and restoration are reserved for Phase 7.
    #[error("session restore is not yet supported (Phase 7)")]
    RestoreNotSupported,
}

/// Thread-safe manager for all active agent sessions.
///
/// The manager owns the registry and shares one scheduler among every runtime
/// it creates.
pub struct SessionManager {
    /// Map from session identifier to the corresponding runtime handle.
    sessions: RwLock<HashMap<SessionId, Arc<SessionRuntime>>>,
    /// Permits retained for the lifetime of active sessions.
    session_permits: RwLock<HashMap<SessionId, OwnedSemaphorePermit>>,
    /// Global concurrency throttle shared with all managed runtimes.
    scheduler: Arc<Scheduler>,
}

impl SessionManager {
    /// Creates a new, empty manager with the given scheduler.
    pub fn new(scheduler: Arc<Scheduler>) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            session_permits: RwLock::new(HashMap::new()),
            scheduler,
        }
    }

    /// Creates and stores a new agent session.
    ///
    /// This waits for a session slot, constructs the runtime with the shared
    /// scheduler, then records both the runtime and its permit. The permit is
    /// released when close_session removes the session.
    ///
    /// A supervisor observes the root task. If it ends with a JoinError
    /// (including a panic), it marks only that runtime as failed; other
    /// sessions and the process continue running.
    pub async fn create_session(
        &self,
        backend: Arc<dyn ExecutionBackend>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        root_toolset: AgentToolset,
    ) -> Arc<SessionRuntime> {
        let session_permit = self.scheduler.acquire_session_permit().await;
        let session_id = SessionId::new();
        let runtime = Arc::new(SessionRuntime::new_with_scheduler(
            session_id,
            backend,
            tool_registry,
            workspace,
            event_sink,
            root_toolset,
            self.scheduler.clone(),
        ));

        self.sessions.write().await.insert(session_id, runtime.clone());
        self.session_permits
            .write()
            .await
            .insert(session_id, session_permit);

        if let Some(handle) = runtime.take_root_task_handle() {
            let runtime_for_supervisor = runtime.clone();
            tokio::spawn(async move {
                if let Err(join_err) = handle.await {
                    tracing::error!(%session_id, error = %join_err, "session root task failed");
                    runtime_for_supervisor.mark_failed(join_err.to_string());
                }
            });
        }

        runtime
    }

    /// Returns a cloned handle for an active session, or None when absent.
    pub async fn session_handle(&self, id: SessionId) -> Option<Arc<SessionRuntime>> {
        self.sessions.read().await.get(&id).cloned()
    }

    /// Returns a receiver for events emitted by session id.
    ///
    /// Each runtime has a separate event bus and broadcast channel, so this
    /// receiver cannot observe events from another session.
    ///
    /// # Errors
    ///
    /// Returns NotFound when id is not active.
    pub async fn subscribe(
        &self,
        id: SessionId,
    ) -> Result<broadcast::Receiver<AgentEventEnvelope>, SessionManagerError> {
        self.session_handle(id)
            .await
            .map(|runtime| runtime.event_bus.subscribe())
            .ok_or(SessionManagerError::NotFound(id))
    }

    /// Closes a session, signals cancellation, and releases its session slot.
    ///
    /// # Errors
    ///
    /// Returns NotFound when id is not active.
    pub async fn close_session(&self, id: SessionId) -> Result<(), SessionManagerError> {
        let removed = self.sessions.write().await.remove(&id);
        match removed {
            Some(runtime) => {
                runtime.shutdown();
                // Dropping the owned permit makes a scheduler slot available.
                self.session_permits.write().await.remove(&id);
                Ok(())
            }
            None => Err(SessionManagerError::NotFound(id)),
        }
    }

    /// Restores a previously persisted session (not yet supported).
    pub async fn restore_session(
        &self,
        _id: SessionId,
    ) -> Result<Arc<SessionRuntime>, SessionManagerError> {
        Err(SessionManagerError::RestoreNotSupported)
    }

    /// Returns a point-in-time snapshot of active session IDs in unspecified
    /// order.
    pub async fn active_session_ids(&self) -> Vec<SessionId> {
        self.sessions.read().await.keys().copied().collect()
    }
}

impl Default for SessionManager {
    /// Creates a manager with default global concurrency ceilings.
    fn default() -> Self {
        Self::new(Arc::new(Scheduler::new(Default::default())))
    }
}
