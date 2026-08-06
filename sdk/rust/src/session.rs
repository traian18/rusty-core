//! Ergonomic wrapper around an active or restored session handle.

use harness_engine::SessionHandle;
use harness_protocol::commands::PermissionDecision;
use harness_protocol::ids::{PermissionId, SessionId};

use crate::error::SdkError;
use crate::events::EventStream;

/// A live handle to one session's root agent.
///
/// `Session` re-exposes [`harness_engine::SessionHandle`]'s control surface
/// (`send`, `cancel`, `resolve_permission`, `events`) with SDK error types,
/// and derefs to the underlying handle for engine-native calls
/// (`snapshot()`, `context_inspection()`) that this SDK does not yet wrap
/// with its own types.
///
/// Construct one from a raw [`SessionHandle`] (e.g. returned by
/// `client.session().integration(..)?.start().await?`) with
/// [`Session::from`], or obtain one already wrapped from
/// [`crate::Client::restore_session`].
///
pub struct Session {
    inner: SessionHandle,
}

impl Session {
    /// Send a prompt, starting a new run if the session is idle.
    ///
    /// Busy-session admission semantics (reject vs. steer vs. queue) are not
    /// yet a stable contract at the engine layer; see `sdk_plan.md` SDK-101.
    pub async fn send(&self, prompt: &str) -> Result<(), SdkError> {
        self.inner.send(prompt).await.map_err(Into::into)
    }

    /// Inject input at the active run's next safe command boundary.
    pub async fn steer(&self, prompt: &str) -> Result<(), SdkError> {
        self.inner.steer(prompt).await.map_err(Into::into)
    }

    /// Queue input FIFO to run after the active run completes.
    pub async fn follow_up(&self, prompt: &str) -> Result<(), SdkError> {
        self.inner.follow_up(prompt).await.map_err(Into::into)
    }

    /// Cancel the active run, if any.
    pub async fn cancel(&self) -> Result<(), SdkError> {
        self.inner.cancel().await.map_err(Into::into)
    }

    /// Close this session permanently and release its scheduler slot.
    pub async fn close(&self) -> Result<(), SdkError> {
        self.inner.close().await.map_err(Into::into)
    }

    /// Resolve a pending tool-call permission request.
    pub async fn resolve_permission(
        &self,
        id: PermissionId,
        decision: PermissionDecision,
    ) -> Result<(), SdkError> {
        self.inner
            .resolve_permission(id, decision)
            .await
            .map_err(Into::into)
    }

    /// The identifier of this session.
    pub fn session_id(&self) -> SessionId {
        self.inner.session_id()
    }

    /// Subscribe to the ordered event stream for this session.
    ///
    /// Multiple independent streams may be created; each sees every event
    /// broadcast from the point of subscription onward.
    pub fn events(&self) -> EventStream {
        EventStream::new(self.inner.subscribe())
    }

    /// Consume this wrapper and return the underlying engine handle, for
    /// applications that need engine-native APIs this SDK does not yet
    /// wrap.
    pub fn into_handle(self) -> SessionHandle {
        self.inner
    }
}

impl From<SessionHandle> for Session {
    fn from(inner: SessionHandle) -> Self {
        Self { inner }
    }
}

impl std::ops::Deref for Session {
    type Target = SessionHandle;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
