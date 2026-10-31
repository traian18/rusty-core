//! The manager's contract methods as inherent ones, so callers need not import the traits.

use super::manager::SessionManager;
use std::sync::Arc;
use std::time::Duration;

use rusty_protocol::events::AgentEventEnvelope;
use rusty_protocol::ids::SessionId;
use rusty_protocol::tools::AgentToolset;
use tokio::sync::broadcast;

use crate::integration::IntegrationRegistry;

use crate::session_manager::contracts::{SessionLifecycle, SessionRegistry, SessionRestorer};
use crate::session_manager::errors::SessionManagerError;

use crate::session_runtime::SessionRuntime;
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

impl SessionManager {
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
