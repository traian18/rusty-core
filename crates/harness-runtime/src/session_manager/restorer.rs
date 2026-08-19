//! Durable session restoration engine.
//!
//! Validates trailing events, migrates snapshot schemas, resolves host dependencies,
//! and reconstructs execution backends without replaying side effects.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::OwnedSemaphorePermit;

use harness_protocol::backend::BackendReference;
use harness_protocol::ids::SessionId;
use harness_session_store::{
    migrate_snapshot, replay_snapshot, GapPolicy, HostDependencyResolver, ReplayValidator,
    RestorePolicy, SessionStore,
};

use crate::integration::IntegrationRegistry;
use crate::restore::HostRestoreResolver;
use crate::scheduler::Scheduler;
use crate::session_manager::errors::SessionManagerError;
use crate::session_runtime::SessionRuntime;
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
        let stored = self.store.load_session(id).await?;
        let trailing = ReplayValidator::new(GapPolicy::AllowEphemeralHoles).validate(&stored)?;
        let snapshot = stored.snapshot.ok_or(SessionManagerError::NoSnapshot(id))?;
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
        let default_backend =
            backends
                .remove(&root_key)
                .ok_or_else(|| SessionManagerError::BackendCreation {
                    integration: root.backend.reference.integration.to_string(),
                    message: "root agent's backend was not re-created".into(),
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
            Some(self.store.clone()),
        ));

        Ok((runtime, session_permit))
    }
}

/// Computes a unique cache key for a backend reference by integration name and configuration.
pub fn backend_reference_key(reference: &BackendReference) -> String {
    format!("{}::{}", reference.integration, reference.configuration)
}
