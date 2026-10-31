//! Durable session restoration engine.
//!
//! Validates trailing events, migrates snapshot schemas, resolves host dependencies,
//! and reconstructs execution backends without replaying side effects.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::OwnedSemaphorePermit;

use rusty_protocol::backend::BackendReference;
use rusty_protocol::ids::SessionId;
use rusty_session_store::{
    migrate_snapshot, replay_snapshot, DurableSessionSnapshot, GapPolicy, HostDependencyResolver,
    ReplayValidator, RestorePolicy, SessionStore, StoredAgentState,
};

use crate::integration::IntegrationRegistry;
use crate::restore::HostRestoreResolver;
use crate::scheduler::Scheduler;
use crate::session_manager::errors::SessionManagerError;
use crate::session_runtime::{RestoreError, SessionHost, SessionRuntime};
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

/// Orchestrates session recovery from persistent storage into a live runtime.
pub struct SessionRestorerEngine {
    store: Arc<dyn SessionStore>,
    restore_policy: RestorePolicy,
    scheduler: Arc<Scheduler>,
}

impl SessionRestorerEngine {
    /// Creates a new `SessionRestorerEngine` with the required storage, policy, and scheduler.
    pub fn new(
        store: Arc<dyn SessionStore>,
        restore_policy: RestorePolicy,
        scheduler: Arc<Scheduler>,
    ) -> Self {
        Self {
            store,
            restore_policy,
            scheduler,
        }
    }

    /// Restores a stored session into an active runtime instance and acquires a scheduler permit.
    ///
    /// The restoration workflow:
    /// 1. Loads the stored session state (snapshot and trailing events) from the store.
    /// 2. Validates trailing durable events with [`ReplayValidator`].
    /// 3. Migrates snapshot version to current runtime schema.
    /// 4. Replays trailing events onto snapshot state.
    /// 5. Resolves host dependencies (workspace & integrations) against the restore policy.
    /// 6. Recreates all agent execution backends via the [`IntegrationRegistry`].
    /// 7. Acquires a capacity permit from the [`Scheduler`].
    /// 8. Constructs the restored [`SessionRuntime`].
    pub async fn restore(
        &self,
        id: SessionId,
        integrations: Arc<IntegrationRegistry>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
    ) -> Result<(Arc<SessionRuntime>, OwnedSemaphorePermit), SessionManagerError> {
        let snapshot = self.load_snapshot(id).await?;
        self.check_host_dependencies(id, &snapshot, workspace.as_ref(), &integrations)
            .await?;
        let mut backends = recreate_backends(&snapshot.agents, &integrations).await?;
        let default_backend = take_root_backend(id, &snapshot, &mut backends)?;

        let session_permit = self.scheduler.acquire_session_permit().await;
        let host = SessionHost {
            scheduler: self.scheduler.clone(),
            integrations,
            session_store: Some(self.store.clone()),
            ..SessionHost::new(id, default_backend, tool_registry, workspace, event_sink)
        };
        let runtime =
            SessionRuntime::from_stored(host, snapshot.agents).map_err(|error| match error {
                RestoreError::Behavior { agent, error } => {
                    SessionManagerError::BehaviorRestore { agent, error }
                }
                RestoreError::NoRootAgent => SessionManagerError::NoSnapshot(id),
            })?;
        Ok((Arc::new(runtime), session_permit))
    }

    /// The stored snapshot with its trailing durable events replayed onto it.
    async fn load_snapshot(
        &self,
        id: SessionId,
    ) -> Result<DurableSessionSnapshot, SessionManagerError> {
        let stored = self.store.load_session(id).await?;
        let trailing = ReplayValidator::new(GapPolicy::AllowEphemeralHoles).validate(&stored)?;
        let snapshot = stored.snapshot.ok_or(SessionManagerError::NoSnapshot(id))?;
        let snapshot = migrate_snapshot(snapshot)?;
        Ok(replay_snapshot(snapshot, &trailing)?)
    }

    /// Refuses the restore when the host cannot supply what the session depended on.
    async fn check_host_dependencies(
        &self,
        id: SessionId,
        snapshot: &DurableSessionSnapshot,
        workspace: &dyn Workspace,
        integrations: &Arc<IntegrationRegistry>,
    ) -> Result<(), SessionManagerError> {
        let resolver = HostRestoreResolver::new(workspace, integrations.clone());
        let report = resolver.resolve(id, &snapshot.metadata).await;
        rusty_session_store::assess_restore(&report, self.restore_policy).inspect_err(
            |_error| {
                tracing::error!(
                    %id,
                    missing = ?report.missing,
                    "restore refused: host dependencies could not be resolved"
                );
            },
        )?;
        Ok(())
    }
}

/// One backend per distinct backend reference of the snapshot's agents, recreated from the
/// persisted configuration.
async fn recreate_backends(
    agents: &[StoredAgentState],
    integrations: &IntegrationRegistry,
) -> Result<HashMap<String, Arc<dyn ExecutionBackend>>, SessionManagerError> {
    let mut backends: HashMap<String, Arc<dyn ExecutionBackend>> = HashMap::new();
    for agent in agents {
        let key = backend_reference_key(&agent.backend.reference);
        if backends.contains_key(&key) {
            continue;
        }
        let integration_id = registered_integration(agent, integrations)?;
        let backend = integrations
            .create(&integration_id, agent.backend_config.clone())
            .await
            .map_err(|error| SessionManagerError::BackendCreation {
                integration: integration_id.clone(),
                message: error.to_string(),
            })?;
        backends.insert(key, backend);
    }
    Ok(backends)
}

/// The id of the registered integration that serves the agent's backend: the persisted one, or
/// the one that registers the same backend by name.
fn registered_integration(
    agent: &StoredAgentState,
    integrations: &IntegrationRegistry,
) -> Result<String, SessionManagerError> {
    let persisted_id = agent.backend.reference.integration.to_string();
    let failed = |message: String| SessionManagerError::BackendCreation {
        integration: persisted_id.clone(),
        message,
    };
    match integrations.get(&persisted_id) {
        Ok(Some(_)) => Ok(persisted_id.clone()),
        Ok(None) => integrations
            .id_for_descriptor_name(&agent.backend.descriptor.name)
            .map_err(|error| failed(error.to_string()))?
            .ok_or_else(|| {
                failed(format!(
                    "no registered integration matches backend {}",
                    agent.backend.descriptor.name
                ))
            }),
        Err(error) => Err(failed(error.to_string())),
    }
}

/// The root agent's backend, which becomes the session's default.
fn take_root_backend(
    id: SessionId,
    snapshot: &DurableSessionSnapshot,
    backends: &mut HashMap<String, Arc<dyn ExecutionBackend>>,
) -> Result<Arc<dyn ExecutionBackend>, SessionManagerError> {
    let root = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id == snapshot.root_agent_id)
        .ok_or(SessionManagerError::NoSnapshot(id))?;
    backends
        .remove(&backend_reference_key(&root.backend.reference))
        .ok_or_else(|| SessionManagerError::BackendCreation {
            integration: root.backend.reference.integration.to_string(),
            message: "root agent's backend was not re-created".into(),
        })
}

/// Computes a unique cache key for a backend reference by integration name and configuration.
pub fn backend_reference_key(reference: &BackendReference) -> String {
    format!("{}::{}", reference.integration, reference.configuration)
}
