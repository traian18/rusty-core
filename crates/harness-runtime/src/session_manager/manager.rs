//! Concrete session manager implementation coordinating registry, lifecycle, and restore.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;

use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::SessionId;
use harness_protocol::tools::AgentToolset;
use harness_session_store::{RestorePolicy, SessionStore};

use crate::integration::IntegrationRegistry;
use crate::scheduler::{Scheduler, SchedulerConfig};
use crate::session_manager::contracts::{SessionLifecycle, SessionRegistry, SessionRestorer};
use crate::session_manager::errors::SessionManagerError;
use crate::session_manager::registry::ActiveSessionRegistry;
use crate::session_manager::restorer::SessionRestorerEngine;
use crate::session_manager::supervisor::SessionSupervisor;
use crate::session_runtime::SessionRuntime;
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

/// Central session coordinator managing multi-session lifecycle, durable restoration,
/// and runtime capacity.
///
/// Implements the [`SessionRegistry`], [`SessionLifecycle`], and [`SessionRestorer`] contracts
/// while encapsulating underlying sub-systems (storage, registry, scheduling, supervision).
pub struct SessionManager {
    registry: ActiveSessionRegistry,
    scheduler: Arc<Scheduler>,
    store: Option<Arc<dyn SessionStore>>,
    restore_policy: RestorePolicy,
}

impl SessionManager {
    /// Creates a new `SessionManager` with the provided scheduler and no session store.
    pub fn new(scheduler: Arc<Scheduler>) -> Self {
        Self::new_with_store(scheduler, None)
    }

    /// Creates a new `SessionManager` with a scheduler and an optional persistent session store.
    pub fn new_with_store(scheduler: Arc<Scheduler>, store: Option<Arc<dyn SessionStore>>) -> Self {
        Self {
            registry: ActiveSessionRegistry::new(),
            scheduler,
            store,
            restore_policy: RestorePolicy::RejectMissing,
        }
    }

    /// Builder method to configure a persistent session store.
    pub fn with_session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Builder method to configure the host dependency restore policy.
    pub fn with_restore_policy(mut self, policy: RestorePolicy) -> Self {
        self.restore_policy = policy;
        self
    }

    /// Returns the shared scheduler used for permit admission and throttling.
    pub fn scheduler(&self) -> Arc<Scheduler> {
        self.scheduler.clone()
    }

    /// Returns a reference to the internal active session registry.
    pub fn registry(&self) -> &ActiveSessionRegistry {
        &self.registry
    }

    // -------------------------------------------------------------------------
    // Inherent convenience methods mirroring trait contracts for ergonomics
    // and full backward compatibility.
    // -------------------------------------------------------------------------

    /// Number of sessions currently tracked as active by this manager.
    pub async fn active_session_count(&self) -> usize {
        <Self as SessionRegistry>::active_session_count(self).await
    }

    /// Retrieves an active session runtime handle by its session ID.
    pub async fn session_handle(&self, id: SessionId) -> Option<Arc<SessionRuntime>> {
        <Self as SessionRegistry>::session_handle(self, id).await
    }

    /// Subscribes to the event stream of an active session.
    pub async fn subscribe(
        &self,
        id: SessionId,
    ) -> Result<broadcast::Receiver<AgentEventEnvelope>, SessionManagerError> {
        <Self as SessionRegistry>::subscribe(self, id).await
    }

    /// Returns a list of all active session IDs.
    pub async fn active_session_ids(&self) -> Vec<SessionId> {
        <Self as SessionRegistry>::active_session_ids(self).await
    }

    /// Creates and starts a new session with bounded-wait scheduler admission.
    pub async fn create_session(
        &self,
        backend: Arc<dyn ExecutionBackend>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        root_toolset: AgentToolset,
    ) -> Result<Arc<SessionRuntime>, SessionManagerError> {
        <Self as SessionLifecycle>::create_session(
            self,
            backend,
            tool_registry,
            workspace,
            event_sink,
            root_toolset,
        )
        .await
    }

    /// Writes a final checkpoint and gracefully shuts down an active session.
    pub async fn close_session(&self, id: SessionId) -> Result<(), SessionManagerError> {
        <Self as SessionLifecycle>::close_session(self, id).await
    }

    /// Closes all active sessions concurrently within the specified grace period.
    pub async fn close_all_sessions(&self, grace_period: Duration) -> Vec<SessionId> {
        <Self as SessionLifecycle>::close_all_sessions(self, grace_period).await
    }

    /// Restores a stored session without replaying external side effects.
    pub async fn restore_session(
        &self,
        id: SessionId,
        integrations: Arc<IntegrationRegistry>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
    ) -> Result<Arc<SessionRuntime>, SessionManagerError> {
        <Self as SessionRestorer>::restore_session(
            self,
            id,
            integrations,
            tool_registry,
            workspace,
            event_sink,
        )
        .await
    }
}

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

        let restorer = SessionRestorerEngine::new(
            store.clone(),
            self.restore_policy,
            self.scheduler.clone(),
        );

        let (runtime, session_permit) = restorer
            .restore(
                id,
                integrations,
                tool_registry,
                workspace,
                event_sink,
            )
            .await?;

        self.registry
            .register(id, runtime.clone(), session_permit)
            .await;
        SessionSupervisor::supervise(id, &runtime);
        Ok(runtime)
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new(Arc::new(Scheduler::new(SchedulerConfig::default())))
    }
}
