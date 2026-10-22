//! The live session handle: commands that go through the session's client.
//!
//! User input lives in [`input`], and calls that reach the session runtime
//! through the [`SessionManager`] in [`runtime`].

mod input;
mod runtime;

use std::sync::Arc;

use tokio::sync::broadcast;

use harness_protocol::commands::PermissionDecision;
use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::{PermissionId, SessionId};
use harness_runtime::session_client::{SessionClient, SessionSnapshot};
use harness_runtime::session_manager::SessionManager;
use harness_runtime::session_runtime::{SessionCommand, SessionRuntime};

use super::HarnessError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextInspection {
    pub generation: u64,
    pub estimated_tokens: Option<u64>,
    pub checkpoint: Option<String>,
    pub covered_through: Option<String>,
    pub pinned_items: usize,
    pub last_compacted_at: Option<String>,
}

/// Handle to a live session.
pub struct SessionHandle {
    client: SessionClient,
    pub(crate) session_id: SessionId,
    pub(crate) session_manager: Arc<SessionManager>,
    pub(crate) orchestration: Option<crate::orchestration::OrchestrationConfig>,
    /// Profiles this session resolves references against (host registry
    /// plus any workspace profiles loaded at start).
    pub(crate) profiles: Arc<harness_core::behavior::ProfileRegistry>,
    /// Whether this session's profiles may run commands.
    pub(crate) command_trust: crate::profiles::CommandTrust,
}

impl SessionHandle {
    /// The handle [`SessionBuilder::start`](super::SessionBuilder::start)
    /// returns for the session it just created.
    pub(super) fn new(
        client: SessionClient,
        session_id: SessionId,
        session_manager: Arc<SessionManager>,
        orchestration: Option<crate::orchestration::OrchestrationConfig>,
        profiles: Arc<harness_core::behavior::ProfileRegistry>,
        command_trust: crate::profiles::CommandTrust,
    ) -> Self {
        Self {
            client,
            session_id,
            session_manager,
            orchestration,
            profiles,
            command_trust,
        }
    }

    /// Wraps an already-running session runtime as a live [`SessionHandle`].
    ///
    /// This is the restore path's counterpart to
    /// [`SessionBuilder::start`](super::SessionBuilder::start): instead of creating
    /// a fresh session, it exposes a runtime that was rebuilt from a
    /// persisted snapshot (see
    /// [`Harness::restore_session`](crate::Harness::restore_session)) through
    /// the same handle API.
    pub(crate) fn from_runtime(
        runtime: Arc<SessionRuntime>,
        session_manager: Arc<SessionManager>,
        orchestration: Option<crate::orchestration::OrchestrationConfig>,
        profiles: Arc<harness_core::behavior::ProfileRegistry>,
    ) -> Self {
        let session_id = runtime.session_id;
        // A restored agent keeps the trust it was installed with; new
        // installs through this handle get none unless re-granted.
        Self::new(
            SessionClient::new(runtime),
            session_id,
            session_manager,
            orchestration,
            profiles,
            Default::default(),
        )
    }

    /// Switch the root agent to another registered profile (e.g. plan →
    /// build). Takes effect immediately when the agent is idle; during a run
    /// it applies before the next model request, and the model is told its
    /// mode changed.
    pub async fn set_behavior_profile(
        &self,
        reference: harness_core::behavior::ProfileRef,
    ) -> Result<(), HarnessError> {
        let bundle =
            crate::profiles::resolve_bundle(&self.profiles, Some(&reference), self.command_trust)?;
        self.client.send(bundle.command()).await?;
        Ok(())
    }

    /// Update this session's default model/execution params.
    ///
    /// Applied as a partial update — fields left unset in `params` keep
    /// their previous value (see `ExecutionParams::merge_over`). Takes
    /// effect starting with the root agent's next model request -- the next
    /// prompt/steer/follow-up, or the active run's next turn -- and never
    /// mutates an already-in-flight request. Use this both to change the session's
    /// standing default (e.g. "switch this session to opus") and, sent
    /// immediately before one `send`, as a one-off override for the next
    /// run only if you follow it with another call reverting the field.
    pub async fn set_execution_params(
        &self,
        params: harness_protocol::backend::ExecutionParams,
    ) -> Result<(), HarnessError> {
        self.client
            .send(SessionCommand::ConfigureExecution(params))
            .await?;
        Ok(())
    }

    /// Cancel the active run without closing the session.
    ///
    /// Queued follow-ups are preserved and the handle remains usable for
    /// future prompts.
    pub async fn cancel(&self) -> Result<(), HarnessError> {
        self.client.cancel_run().await?;
        Ok(())
    }

    /// Pause the session's active run.
    ///
    /// Unlike [`cancel`](Self::cancel) this is recoverable: the agent stops
    /// at its next transition point and holds its run state, so
    /// [`resume`](Self::resume) continues the same run rather than starting a
    /// new one. Pausing an already-paused, cancelled, or failed agent is a
    /// no-op — the state machine rejects the transition and emits nothing.
    ///
    /// Note that this does not abort an in-flight backend request; a run
    /// waiting on the model pauses once that request settles.
    pub async fn pause(&self) -> Result<(), HarnessError> {
        self.client.send(SessionCommand::Pause).await?;
        Ok(())
    }

    /// Resume a paused session, continuing the run that was interrupted.
    ///
    /// A no-op unless the agent is actually paused.
    pub async fn resume(&self) -> Result<(), HarnessError> {
        self.client.send(SessionCommand::Resume).await?;
        Ok(())
    }

    /// Answer a pending tool permission request.
    pub async fn resolve_permission(
        &self,
        id: PermissionId,
        decision: PermissionDecision,
    ) -> Result<(), HarnessError> {
        self.client.resolve_permission(id, decision).await?;
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEventEnvelope> {
        self.client.subscribe()
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        self.client.snapshot()
    }

    pub fn context_inspection(&self) -> ContextInspection {
        let context = self.client.snapshot().context;
        ContextInspection {
            generation: context.generation,
            estimated_tokens: context.estimated_tokens,
            checkpoint: context.checkpoint,
            covered_through: context.covered_through,
            pinned_items: context.pinned_items,
            last_compacted_at: context.last_compacted_at,
        }
    }
}
