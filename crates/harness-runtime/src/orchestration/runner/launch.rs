//! Starting a run: fresh, resumed from a snapshot, or started and awaited.

use harness_core::orchestration::{
    check_restorable, OrchestrationCommand, OrchestrationNodeKind, OrchestrationRunId,
    OrchestrationRunState, ToolScope,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{
    run_loop, OrchestrationHandle, OrchestrationRunOutput, OrchestrationRunner,
    OrchestrationRuntimeError,
};

impl OrchestrationRunner {
    /// Start a new run. Fails without side effects if the definition
    /// references unregistered tools.
    pub fn start(
        &self,
        run_id: OrchestrationRunId,
        input: Value,
    ) -> Result<OrchestrationHandle, OrchestrationRuntimeError> {
        self.check_tool_references()?;
        Ok(self.spawn(
            self.new_state(run_id),
            0,
            0,
            OrchestrationCommand::Start { input },
            CancellationToken::new(),
        ))
    }

    /// Resume a run from its last committed snapshot. Completed steps are not
    /// re-run; an agent attempt that was in flight fails closed with
    /// `INDETERMINATE_ATTEMPT` because its side effects are unknown.
    pub async fn resume(
        &self,
        run_id: OrchestrationRunId,
    ) -> Result<OrchestrationHandle, OrchestrationRuntimeError> {
        let store = self
            .store
            .as_ref()
            .ok_or(OrchestrationRuntimeError::NoStore)?;
        let snapshot = store
            .load_snapshot(&run_id)
            .await?
            .ok_or_else(|| OrchestrationRuntimeError::RunNotFound(run_id.clone()))?;
        if snapshot.state.status.is_terminal() {
            return Err(OrchestrationRuntimeError::RunFinished(run_id));
        }
        check_restorable(&self.compiled, &snapshot.state)
            .map_err(|error| OrchestrationRuntimeError::Restore(error.message))?;
        self.check_tool_references()?;
        Ok(self.spawn(
            snapshot.state,
            snapshot.last_sequence,
            snapshot.elapsed_ms,
            OrchestrationCommand::Recover,
            CancellationToken::new(),
        ))
    }

    /// Start a run and wait for it, cancelling when `cancellation` fires.
    pub async fn run(
        &self,
        run_id: OrchestrationRunId,
        input: Value,
        cancellation: CancellationToken,
    ) -> Result<OrchestrationRunOutput, OrchestrationRuntimeError> {
        self.check_tool_references()?;
        self.spawn(
            self.new_state(run_id),
            0,
            0,
            OrchestrationCommand::Start { input },
            cancellation,
        )
        .wait()
        .await
    }

    fn new_state(&self, run_id: OrchestrationRunId) -> OrchestrationRunState {
        let mut state = OrchestrationRunState::new(run_id, &self.compiled);
        state.options = self.options.clone();
        state
    }

    pub(super) fn check_tool_references(&self) -> Result<(), OrchestrationRuntimeError> {
        let Some(available) = &self.available_tools else {
            return Ok(());
        };
        let mut missing: Vec<_> = self
            .compiled
            .nodes
            .values()
            .filter_map(|node| match &node.kind {
                OrchestrationNodeKind::Agent(config) => match &config.tools {
                    ToolScope::AllowList(tools) => Some(tools),
                    _ => None,
                },
                _ => None,
            })
            .flatten()
            .filter(|tool| available.binary_search(tool).is_err())
            .cloned()
            .collect();
        missing.sort();
        missing.dedup();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(OrchestrationRuntimeError::UnknownTools(missing))
        }
    }

    pub(super) fn spawn(
        &self,
        state: OrchestrationRunState,
        last_sequence: u64,
        elapsed_before_ms: u64,
        first: OrchestrationCommand,
        cancellation: CancellationToken,
    ) -> OrchestrationHandle {
        run_loop::spawn(
            self.clone(),
            state,
            last_sequence,
            elapsed_before_ms,
            first,
            cancellation,
        )
    }
}
