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

use harness_protocol::backend::BackendReference;
use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::SessionId;
use harness_protocol::tools::AgentToolset;
use harness_session_store::{SessionStore, StoreError};

use crate::integration::IntegrationRegistry;
use crate::scheduler::Scheduler;
use crate::session_runtime::SessionRuntime;
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

/// Errors that can occur during session management operations.
#[derive(Debug, thiserror::Error)]
pub enum SessionManagerError {
    /// The requested session ID does not correspond to any active session.
    #[error("session {0} not found")]
    NotFound(SessionId),

    /// Session restore was requested but no durable store is configured.
    #[error("session restore requires a configured session store")]
    RestoreNotSupported,

    /// The durable session store failed to load or write session data.
    #[error("session store error: {0}")]
    Store(#[from] StoreError),

    /// The stored session exists but carries no snapshot checkpoint to
    /// restore from (only an event log).
    #[error("session {0} has no stored snapshot to restore from")]
    NoSnapshot(SessionId),

    /// A stored agent's backend could not be re-created through the
    /// integration registry.
    #[error("failed to re-create backend for integration {integration}: {message}")]
    BackendCreation {
        /// The integration family that failed to construct a backend.
        integration: String,
        /// The factory's error description.
        message: String,
    },
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
    /// Optional durable session store.
    ///
    /// Consulted by [`restore_session`](Self::restore_session) to reload a
    /// persisted session, and handed back into every restored runtime so its
    /// agent runners keep routing `Persist` effects through the store. Set at
    /// construction time via [`new_with_store`](Self::new_with_store) or the
    /// builder-style [`with_session_store`](Self::with_session_store); `None`
    /// disables persistence and makes restore fail with
    /// [`SessionManagerError::RestoreNotSupported`].
    store: Option<Arc<dyn SessionStore>>,
}

impl SessionManager {
    /// Creates a new, empty manager with the given scheduler and no session
    /// store (restore is unavailable until a store is configured).
    pub fn new(scheduler: Arc<Scheduler>) -> Self {
        Self::new_with_store(scheduler, None)
    }

    /// Creates a new, empty manager with the given scheduler and an optional
    /// durable session store.
    ///
    /// The store is retained for [`restore_session`](Self::restore_session)
    /// and is threaded into every restored runtime so `Persist` effects
    /// continue to be persisted after a restore.
    pub fn new_with_store(
        scheduler: Arc<Scheduler>,
        store: Option<Arc<dyn SessionStore>>,
    ) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            session_permits: RwLock::new(HashMap::new()),
            scheduler,
            store,
        }
    }

    /// Builder-style setter for the durable session store.
    ///
    /// Mirrors [`crate::agent_runner::AgentRunner::with_session_store`] so the
    /// manager can be assembled fluently. The store is consulted by
    /// [`restore_session`](Self::restore_session) and by restored runners.
    pub fn with_session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.store = Some(store);
        self
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
            self.store.clone(),
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

    /// Restores a previously persisted session from the configured store.
    ///
    /// # Flow
    ///
    /// 1. `store.load_session(id)` fetches the latest durable snapshot (plus
    ///    any durable events appended after it).
    /// 2. For every distinct [`BackendReference`] in the snapshot, a fresh
    ///    [`ExecutionBackend`] is created through
    ///    [`IntegrationRegistry::create`] using the resolved, non-secret
    ///    config JSON that was persisted alongside each agent's binding at
    ///    snapshot time — no separate configuration registry is needed.
    /// 3. The session runtime is rebuilt from the stored agent states via
    ///    [`SessionRuntime::from_stored`], with the store handed back to the
    ///    restored agent runners so `Persist` effects keep flowing.
    ///
    /// A restored session occupies a session slot, is registered with the
    /// manager like any other session, and its root task is supervised for
    /// panics. Restoring an already-active session returns the live handle.
    ///
    /// # Errors
    ///
    /// Returns [`SessionManagerError::RestoreNotSupported`] when no store is
    /// configured, [`SessionManagerError::Store`] when the store cannot load
    /// the session, [`SessionManagerError::NoSnapshot`] when the stored
    /// session has no snapshot checkpoint, and
    /// [`SessionManagerError::BackendCreation`] when an integration factory
    /// cannot re-create one of the stored backends.
    pub async fn restore_session(
        &self,
        id: SessionId,
        integrations: Arc<IntegrationRegistry>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
    ) -> Result<Arc<SessionRuntime>, SessionManagerError> {
        let Some(store) = &self.store else {
            tracing::warn!(%id, "restore_session called without a configured session store");
            return Err(SessionManagerError::RestoreNotSupported);
        };

        // Restoring an already-active session is a no-op returning the live
        // handle, so a client can safely re-issue restore after a reconnect.
        if let Some(existing) = self.session_handle(id).await {
            return Ok(existing);
        }

        let stored = store.load_session(id).await?;
        let snapshot = stored
            .snapshot
            .ok_or(SessionManagerError::NoSnapshot(id))?;

        // Re-create a fresh backend for every distinct BackendReference found
        // in the snapshot. The resolved, non-secret config JSON was persisted
        // next to the agent's binding at snapshot time, so restore does not
        // need a ConfigurationRegistry.
        let mut backends: HashMap<String, Arc<dyn ExecutionBackend>> = HashMap::new();
        for agent in &snapshot.agents {
            let reference = &agent.backend.reference;
            let key = backend_reference_key(reference);
            if backends.contains_key(&key) {
                continue;
            }
            let persisted_id = reference.integration.to_string();
            let integration_id = match integrations.get(&persisted_id) {
                Ok(Some(_)) => persisted_id,
                Ok(None) => integrations
                    .id_for_descriptor_name(&agent.backend.descriptor.name)
                    .map_err(|error| SessionManagerError::BackendCreation {
                        integration: persisted_id.clone(),
                        message: error.to_string(),
                    })?
                    .ok_or_else(|| SessionManagerError::BackendCreation {
                        integration: persisted_id.clone(),
                        message: format!(
                            "no registered integration matches backend {}",
                            agent.backend.descriptor.name
                        ),
                    })?,
                Err(error) => {
                    return Err(SessionManagerError::BackendCreation {
                        integration: persisted_id,
                        message: error.to_string(),
                    });
                }
            };
            let backend = integrations
                .create(&integration_id, agent.backend_config.clone())
                .await
                .map_err(|error| SessionManagerError::BackendCreation {
                    integration: integration_id.clone(),
                    message: error.to_string(),
                })?;
            backends.insert(key, backend);
        }

        // The root agent's backend becomes the session's default backend.
        let root = snapshot
            .agents
            .iter()
            .find(|agent| agent.agent_id == snapshot.root_agent_id)
            .ok_or(SessionManagerError::NoSnapshot(id))?;
        let root_key = backend_reference_key(&root.backend.reference);
        let default_backend = backends.remove(&root_key).ok_or_else(|| {
            SessionManagerError::BackendCreation {
                integration: root.backend.reference.integration.to_string(),
                message: "root agent's backend was not re-created".into(),
            }
        })?;

        // Rebuild the runtime from the snapshot's stored agent states.
        let session_permit = self.scheduler.acquire_session_permit().await;
        let runtime = Arc::new(SessionRuntime::from_stored(
            id,
            snapshot.agents,
            default_backend,
            tool_registry,
            workspace,
            event_sink,
            self.scheduler.clone(),
            integrations,
            Some(store.clone()),
        ));

        self.sessions.write().await.insert(id, runtime.clone());
        self.session_permits
            .write()
            .await
            .insert(id, session_permit);

        if let Some(handle) = runtime.take_root_task_handle() {
            let runtime_for_supervisor = runtime.clone();
            tokio::spawn(async move {
                if let Err(join_err) = handle.await {
                    tracing::error!(%id, error = %join_err, "restored session root task failed");
                    runtime_for_supervisor.mark_failed(join_err.to_string());
                }
            });
        }

        Ok(runtime)
    }

    /// Returns a point-in-time snapshot of active session IDs in unspecified
    /// order.
    pub async fn active_session_ids(&self) -> Vec<SessionId> {
        self.sessions.read().await.keys().copied().collect()
    }
}

/// Stable deduplication key for a [`BackendReference`]: integration +
/// configuration, since the resolved config is keyed per configuration.
fn backend_reference_key(reference: &BackendReference) -> String {
    format!("{}::{}", reference.integration, reference.configuration)
}

impl Default for SessionManager {
    /// Creates a manager with default global concurrency ceilings.
    fn default() -> Self {
        Self::new(Arc::new(Scheduler::new(Default::default())))
    }
}
