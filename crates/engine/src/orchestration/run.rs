//! Running, resuming and retrying a workflow on a session.

use super::*;
use rusty_workflow_core::{DefinitionRef, OrchestrationRunId};
use rusty_workflow_runtime::orchestration::{
    OrchestrationHandle, OrchestrationRunOutput, OrchestrationRunner,
};
use std::sync::Arc;

use crate::{HarnessError, SessionHandle};
use tokio_util::sync::CancellationToken;

impl SessionHandle {
    /// Start a workflow. Every agent-step attempt runs in a fresh, isolated,
    /// tool-scoped session that inherits this session's backend, workspace,
    /// active model, and tool permission policies. Permission requests are
    /// surfaced through the returned handle.
    pub async fn start_orchestration(
        &self,
        request: OrchestrationRequest,
    ) -> Result<OrchestrationHandle, HarnessError> {
        self.orchestration_runner(&request.definition)
            .await?
            .with_options(request.options)
            .start(request.run_id, request.input)
            .map_err(|error| HarnessError::OrchestrationRuntime(error.to_string()))
    }

    /// Start a workflow and wait for its result, cancelling it when
    /// `cancellation` fires.
    pub async fn run_orchestration(
        &self,
        request: OrchestrationRequest,
        cancellation: CancellationToken,
    ) -> Result<OrchestrationRunOutput, HarnessError> {
        self.orchestration_runner(&request.definition)
            .await?
            .with_options(request.options)
            .run(request.run_id, request.input, cancellation)
            .await
            .map_err(|error| HarnessError::OrchestrationRuntime(error.to_string()))
    }

    /// Resume a run from its last durable step boundary (requires a store on
    /// the [`OrchestrationConfig`]). `definition` must name the exact
    /// revision the run started on.
    pub async fn resume_orchestration(
        &self,
        run_id: OrchestrationRunId,
        definition: DefinitionRef,
    ) -> Result<OrchestrationHandle, HarnessError> {
        self.orchestration_runner(&definition)
            .await?
            .resume(run_id)
            .await
            .map_err(|error| HarnessError::OrchestrationRuntime(error.to_string()))
    }

    /// Retry only the failed step of a validated checkpoint, on this session's backend.
    pub async fn retry_orchestration(
        &self,
        request: OrchestrationRequest,
        state: rusty_workflow_core::OrchestrationRunState,
        guidance: String,
    ) -> Result<OrchestrationHandle, HarnessError> {
        self.orchestration_runner(&request.definition)
            .await?
            .retry_failed(state, request.run_id, guidance)
            .map_err(|error| HarnessError::OrchestrationRuntime(error.to_string()))
    }

    pub(super) async fn orchestration_runner(
        &self,
        definition: &DefinitionRef,
    ) -> Result<OrchestrationRunner, HarnessError> {
        let config = self
            .orchestration
            .as_ref()
            .ok_or(HarnessError::OrchestrationNotConfigured)?;
        let parts = Arc::new(RunnerParts {
            session_id: self.session_id,
            session_manager: self.session_manager.clone(),
            config: config.clone(),
            profiles: self.profiles.clone(),
            command_trust: self.command_trust,
        });
        let compiled = Arc::new(parts.resolve(definition)?);
        // Everything the run may reach -- other flows, steps run as flows --
        // must exist, must not call back into itself and must have profiles
        // that resolve, before anything runs.
        parts.check_reachable(&compiled)?;
        parts.runner(compiled, 0).await
    }
}
