//! How a [`SessionManager`] serves its three contracts: registry queries, lifecycle, restoration.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;

use rusty_protocol::events::AgentEventEnvelope;
use rusty_protocol::ids::SessionId;
use rusty_protocol::tools::AgentToolset;

use crate::integration::IntegrationRegistry;

use crate::session_manager::contracts::{SessionLifecycle, SessionRegistry, SessionRestorer};
use crate::session_manager::errors::SessionManagerError;

use crate::session_manager::restorer::SessionRestorerEngine;
use crate::session_manager::supervisor::SessionSupervisor;
use crate::session_runtime::{SessionHost, SessionRuntime};
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

use super::manager::SessionManager;

#[async_trait]
impl SessionRegistry for SessionManager {
    async fn session_handle(&self, id: SessionId) -> Option<Arc<SessionRuntime>> {
        self.registry.get(id).await
    }

    async fn active_session_count(&self) -> usize {
        self.registry.count().await
    }

    async fn active_session_ids(&self) -> Vec<SessionId> {
        self.registry.ids().await
    }

    async fn subscribe(
        &self,
        id: SessionId,
    ) -> Result<broadcast::Receiver<AgentEventEnvelope>, SessionManagerError> {
        self.session_handle(id)
            .await
            .map(|runtime| runtime.event_bus.subscribe())
            .ok_or(SessionManagerError::NotFound(id))
    }
}

#[async_trait]
impl SessionLifecycle for SessionManager {
    async fn create_session(
        &self,
        backend: Arc<dyn ExecutionBackend>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        root_toolset: AgentToolset,
    ) -> Result<Arc<SessionRuntime>, SessionManagerError> {
        let session_permit = self.scheduler.try_acquire_session_permit().await?;
        let session_id = SessionId::new();
        let host = SessionHost {
            scheduler: self.scheduler.clone(),
            session_store: self.store.clone(),
            ..SessionHost::new(session_id, backend, tool_registry, workspace, event_sink)
        };
        let runtime = Arc::new(SessionRuntime::start(host, root_toolset, None));

        self.registry
            .register(session_id, runtime.clone(), session_permit)
            .await;

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
                runtime.mark_failed(format!("initial durable checkpoint failed: {error}"));
            }
        }

        SessionSupervisor::supervise(session_id, &runtime);
        Ok(runtime)
    }

    async fn close_session(&self, id: SessionId) -> Result<(), SessionManagerError> {
        let (runtime, _permit) = self
            .registry
            .remove(id)
            .await
            .ok_or(SessionManagerError::NotFound(id))?;

        let checkpoint_result = runtime.checkpoint().await;
        runtime.shutdown();

        if let Err(error) = checkpoint_result {
            tracing::error!(%id, %error, "failed to checkpoint session during close");
            return Err(SessionManagerError::CloseCheckpointFailed {
                session: id,
                error: error.to_string(),
            });
        }
        Ok(())
    }

    async fn close_all_sessions(&self, grace_period: Duration) -> Vec<SessionId> {
        let ids = self.registry.ids().await;
        if ids.is_empty() {
            return Vec::new();
        }

        let closes = ids.iter().map(|&id| {
            let manager_self = self;
            async move {
                match tokio::time::timeout(grace_period, manager_self.close_session(id)).await {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => {
                        tracing::error!(%id, %error, "session did not close cleanly during shutdown drain");
                        Some(id)
                    }
                    Err(_) => {
                        tracing::error!(
                            %id,
                            ?grace_period,
                            "session did not finish closing within the shutdown grace period"
                        );
                        Some(id)
                    }
                }
            }
        });

        futures::future::join_all(closes)
            .await
            .into_iter()
            .flatten()
            .collect()
    }
}

#[async_trait]
impl SessionRestorer for SessionManager {
    async fn restore_session(
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

        if let Some(existing) = self.registry.get(id).await {
            return Ok(existing);
        }

        let restorer =
            SessionRestorerEngine::new(store.clone(), self.restore_policy, self.scheduler.clone());

        let (runtime, session_permit) = restorer
            .restore(id, integrations, tool_registry, workspace, event_sink)
            .await?;

        self.registry
            .register(id, runtime.clone(), session_permit)
            .await;
        SessionSupervisor::supervise(id, &runtime);
        Ok(runtime)
    }
}
