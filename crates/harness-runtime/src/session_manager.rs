//! Session lifecycle management for the harness runtime.
//!
//! RC-300 restore validates and reduces trailing durable events before any
//! provider, tool, permission handler, or external sink is invoked.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{broadcast, OwnedSemaphorePermit, RwLock};

use harness_protocol::backend::BackendReference;
use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::SessionId;
use harness_protocol::tools::AgentToolset;
use harness_session_store::{
    migrate_snapshot, replay_snapshot, GapPolicy, HostDependencyResolver, ReplayError,
    ReplayValidator, RestoreError,
    RestorePolicy, SessionStore, SnapshotVersionError, StoreError,
};

use crate::integration::IntegrationRegistry;
use crate::restore::HostRestoreResolver;
use crate::scheduler::{Scheduler, SchedulerConfig};
use crate::session_runtime::SessionRuntime;
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

#[derive(Debug, thiserror::Error)]
pub enum SessionManagerError {
    #[error("session {0} not found")]
    NotFound(SessionId),
    #[error("session restore requires a configured session store")]
    RestoreNotSupported,
    #[error("session store error: {0}")]
    Store(#[from] StoreError),
    #[error("session {0} has no stored snapshot to restore from")]
    NoSnapshot(SessionId),
    #[error("stored session failed replay validation: {0}")]
    RestoreValidation(#[from] ReplayError),
    #[error("stored snapshot is not migratable: {0}")]
    SnapshotMigration(#[from] SnapshotVersionError),
    #[error("restore rejected: {0}")]
    RestoreRejected(#[from] RestoreError),
    #[error("failed to re-create backend for integration {integration}: {message}")]
    BackendCreation {
        integration: String,
        message: String,
    },
}

pub struct SessionManager {
    sessions: RwLock<HashMap<SessionId, Arc<SessionRuntime>>>,
    session_permits: RwLock<HashMap<SessionId, OwnedSemaphorePermit>>,
    scheduler: Arc<Scheduler>,
    store: Option<Arc<dyn SessionStore>>,
    restore_policy: RestorePolicy,
}

impl SessionManager {
    pub fn new(scheduler: Arc<Scheduler>) -> Self {
        Self::new_with_store(scheduler, None)
    }

    pub fn new_with_store(
        scheduler: Arc<Scheduler>,
        store: Option<Arc<dyn SessionStore>>,
    ) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            session_permits: RwLock::new(HashMap::new()),
            scheduler,
            store,
            restore_policy: RestorePolicy::RejectMissing,
        }
    }

    pub fn with_session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.store = Some(store);
        self
    }

    pub fn with_restore_policy(mut self, policy: RestorePolicy) -> Self {
        self.restore_policy = policy;
        self
    }

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

        self.sessions
            .write()
            .await
            .insert(session_id, runtime.clone());
        self.session_permits
            .write()
            .await
            .insert(session_id, session_permit);

        // RC-302: establish an initial restorable checkpoint before the
        // session accepts meaningful work. This closes the event-only restore
        // gap without inventing a synthetic workspace or backend later.
        if self.store.is_some() {
            if let Err(error) = runtime.checkpoint().await {
                tracing::error!(
                    %session_id,
                    %error,
                    "failed to create initial session checkpoint"
                );
            }
        }

        Self::supervise(session_id, &runtime);
        runtime
    }

    fn supervise(session_id: SessionId, runtime: &Arc<SessionRuntime>) {
        if let Some(handle) = runtime.take_root_task_handle() {
            let runtime_for_supervisor = runtime.clone();
            tokio::spawn(async move {
                if let Err(join_err) = handle.await {
                    tracing::error!(%session_id, error = %join_err, "session root task failed");
                    runtime_for_supervisor.mark_failed(join_err.to_string());
                }
            });
        }
    }

    pub async fn session_handle(&self, id: SessionId) -> Option<Arc<SessionRuntime>> {
        self.sessions.read().await.get(&id).cloned()
    }

    pub async fn subscribe(
        &self,
        id: SessionId,
    ) -> Result<broadcast::Receiver<AgentEventEnvelope>, SessionManagerError> {
        self.session_handle(id)
            .await
            .map(|runtime| runtime.event_bus.subscribe())
            .ok_or(SessionManagerError::NotFound(id))
    }

    /// Writes a final checkpoint before clean shutdown.
    pub async fn close_session(&self, id: SessionId) -> Result<(), SessionManagerError> {
        let runtime = self
            .sessions
            .write()
            .await
            .remove(&id)
            .ok_or(SessionManagerError::NotFound(id))?;

        if let Err(error) = runtime.checkpoint().await {
            tracing::error!(%id, %error, "failed to checkpoint session during close");
        }
        runtime.shutdown();
        self.session_permits.write().await.remove(&id);
        Ok(())
    }

    /// Restores a session without replaying external side effects.
    ///
    /// The latest snapshot is migrated, then every validated trailing event
    /// is reduced onto it before dependency resolution or backend creation.
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

        if let Some(existing) = self.session_handle(id).await {
            return Ok(existing);
        }

        let stored = store.load_session(id).await?;
        let trailing =
            ReplayValidator::new(GapPolicy::AllowEphemeralHoles).validate(&stored)?;
        let snapshot = stored
            .snapshot
            .ok_or(SessionManagerError::NoSnapshot(id))?;
        let snapshot = migrate_snapshot(snapshot)?;
        let snapshot = replay_snapshot(snapshot, &trailing)?;

        let resolver = HostRestoreResolver::new(workspace.as_ref(), integrations.clone());
        let report = resolver.resolve(id, &snapshot.metadata).await;
        harness_session_store::assess_restore(&report, self.restore_policy).inspect_err(|_error| {
            tracing::error!(
                %id,
                missing = ?report.missing,
                "restore refused: host dependencies could not be resolved"
            );
        })?;

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
        Self::supervise(id, &runtime);
        Ok(runtime)
    }

    pub async fn active_session_ids(&self) -> Vec<SessionId> {
        self.sessions.read().await.keys().copied().collect()
    }
}

fn backend_reference_key(reference: &BackendReference) -> String {
    format!("{}::{}", reference.integration, reference.configuration)
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new(Arc::new(Scheduler::new(SchedulerConfig::default())))
    }
}
