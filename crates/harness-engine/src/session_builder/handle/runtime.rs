//! Calls that reach the session runtime through the [`SessionManager`]
//! rather than the session's command channel.
//!
//! [`SessionManager`]: harness_runtime::session_manager::SessionManager

use std::sync::Arc;

use harness_protocol::ids::SessionId;
use harness_runtime::session_runtime::{SessionCommand, SessionRuntime};

use super::SessionHandle;
use crate::session_builder::HarnessError;

impl SessionHandle {
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// The behavior profile the root agent is running under.
    pub async fn behavior_profile(
        &self,
    ) -> Result<harness_core::behavior::ProfileRef, HarnessError> {
        Ok(self.runtime().await?.root_behavior().reference())
    }

    /// Change the execution parameters (e.g. the model) of a session this one
    /// delegates to -- an orchestration step running right now -- from its
    /// next model request. The step keeps the output format it was started
    /// with unless `params` sets one.
    pub async fn set_delegated_execution_params(
        &self,
        session_id: SessionId,
        mut params: harness_protocol::backend::ExecutionParams,
    ) -> Result<(), HarnessError> {
        let step = self
            .runtime()
            .await?
            .delegated_session(session_id)
            .ok_or(HarnessError::SessionUnavailable(session_id))?;
        if params.response_format.is_none() {
            params.response_format = step.root_execution_params().response_format;
        }
        step.send_command(SessionCommand::ConfigureExecution(params))
            .await?;
        Ok(())
    }

    /// Close this session permanently, cancelling active work, stopping event
    /// forwarding, and releasing its scheduler slot.
    pub async fn close(&self) -> Result<(), HarnessError> {
        self.session_manager.close_session(self.session_id).await?;
        Ok(())
    }

    /// This session's runtime, while the manager still holds it.
    async fn runtime(&self) -> Result<Arc<SessionRuntime>, HarnessError> {
        self.session_manager
            .session_handle(self.session_id)
            .await
            .ok_or(HarnessError::SessionUnavailable(self.session_id))
    }
}
