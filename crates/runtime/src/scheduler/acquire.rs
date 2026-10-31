//! Waiting for a slot: sessions, agents, backend requests, tool calls and processes each have their own limit.

use super::{CapacityError, Scheduler};
use std::sync::Arc;
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;

impl Scheduler {
    /// Acquires a permit for creating a new session.
    ///
    /// Blocks until the number of active sessions is below
    /// [`SchedulerConfig::max_active_sessions`].
    pub async fn acquire_session_permit(self: &Arc<Self>) -> OwnedSemaphorePermit {
        self.permits.acquire_session().await
    }

    /// E1: bounded-wait variant of [`Self::acquire_session_permit`] — waits
    /// up to `self.config.admission_timeout` for a slot, then rejects typed
    /// (`CapacityError`) instead of queueing indefinitely. This is the
    /// method `SessionManager::create_session` actually calls; the plain
    /// unconditional-wait `acquire_session_permit` remains available for
    /// callers (mostly tests) that genuinely want to wait forever.
    pub async fn try_acquire_session_permit(
        self: &Arc<Self>,
    ) -> Result<OwnedSemaphorePermit, CapacityError> {
        self.permits.try_acquire_session().await
    }

    /// Acquires a permit for spawning a new agent runner.
    pub async fn acquire_agent_permit(self: &Arc<Self>) -> OwnedSemaphorePermit {
        self.permits.acquire_agent().await
    }

    /// Acquires a permit for executing a backend (LLM) request.
    ///
    /// Blocks until the number of in-flight backend requests is below
    /// [`SchedulerConfig::max_concurrent_backend_requests`].
    pub async fn acquire_backend_permit(self: &Arc<Self>) -> OwnedSemaphorePermit {
        self.permits.acquire_backend().await
    }

    /// Cancellable variant of [`Self::acquire_backend_permit`].
    ///
    /// Returns `None` if `cancel` fires before a permit becomes available.
    pub async fn acquire_backend_permit_cancellable(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Option<OwnedSemaphorePermit> {
        self.permits.acquire_backend_cancellable(cancel).await
    }

    /// Acquires a permit for executing a tool.
    ///
    /// Blocks until the number of in-flight tool executions is below
    /// [`SchedulerConfig::max_concurrent_tool_executions`].
    pub async fn acquire_tool_permit(self: &Arc<Self>) -> OwnedSemaphorePermit {
        self.permits.acquire_tool().await
    }

    /// Cancellable variant of [`Self::acquire_tool_permit`].
    ///
    /// Returns `None` if `cancel` fires before a permit becomes available.
    pub async fn acquire_tool_permit_cancellable(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Option<OwnedSemaphorePermit> {
        self.permits.acquire_tool_cancellable(cancel).await
    }

    /// Acquires a permit for spawning a child process.
    pub async fn acquire_process_permit(self: &Arc<Self>) -> OwnedSemaphorePermit {
        self.permits.acquire_process().await
    }

    /// Cancellable variant of [`Self::acquire_process_permit`].
    ///
    /// Returns `None` if `cancel` fires before a permit becomes available.
    pub async fn acquire_process_permit_cancellable(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Option<OwnedSemaphorePermit> {
        self.permits.acquire_process_cancellable(cancel).await
    }
}
