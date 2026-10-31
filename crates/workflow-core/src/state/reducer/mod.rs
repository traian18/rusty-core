//! The reducer: how a workflow run changes in response to a command.
//!
//! [`apply`] is the table of command to operation; each operation is a function in its own
//! module and returns the effects the runtime should carry out.

mod activate;
mod checkpoint;
mod control;
mod finish;
mod input_wait;
mod lookup;
mod permission;
mod queries;
mod start;

use crate::compiler::CompiledOrchestration;

use crate::state::{
    OrchestrationCommand, OrchestrationEffect, OrchestrationRunState, TransitionError,
};
use activate::retry;
use checkpoint::record_checkpoint;
use control::abort;
use control::cancel;
use control::cancellation_completed;
use control::pause;
use control::recover;
use control::resume;
use finish::fail;
use finish::succeed;
use input_wait::input_required;
use input_wait::input_resolved;
use lookup::transition_error;
use permission::permission_required;
use permission::permission_resolved;
use start::admit;
use start::start;

pub use queries::{check_restorable, ready_steps};

/// Applies one command to a run and returns the effects the runtime should carry out. A finished
/// run accepts no more commands.
pub fn apply(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    command: OrchestrationCommand,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    if state.status.is_terminal() {
        return Err(transition_error(
            "terminal_run",
            "terminal orchestration runs cannot transition",
        ));
    }
    match command {
        OrchestrationCommand::RecordCheckpoint {
            node_id,
            attempt,
            value,
        } => record_checkpoint(compiled, state, node_id, attempt, value),
        OrchestrationCommand::Start { input } => start(compiled, state, input),
        OrchestrationCommand::StepAdmitted { node_id, attempt } => {
            admit(compiled, state, node_id, attempt)
        }
        OrchestrationCommand::StepSucceeded {
            node_id,
            attempt,
            output,
            details,
        } => succeed(compiled, state, node_id, attempt, output, details),
        OrchestrationCommand::StepFailed {
            node_id,
            attempt,
            error,
            details,
        } => fail(compiled, state, node_id, attempt, error, details),
        OrchestrationCommand::PermissionRequired {
            node_id,
            attempt,
            permission_id,
        } => permission_required(compiled, state, node_id, attempt, permission_id),
        OrchestrationCommand::PermissionResolved {
            node_id,
            attempt,
            permission_id,
            approved,
        } => permission_resolved(compiled, state, node_id, attempt, permission_id, approved),
        OrchestrationCommand::InputRequired {
            node_id,
            attempt,
            request,
        } => input_required(compiled, state, node_id, attempt, request),
        OrchestrationCommand::InputResolved {
            node_id,
            attempt,
            request_id,
            response,
        } => input_resolved(compiled, state, node_id, attempt, request_id, response),
        OrchestrationCommand::RetryStep { node_id } => retry(compiled, state, node_id),
        OrchestrationCommand::Pause => pause(state),
        OrchestrationCommand::Resume => resume(state),
        OrchestrationCommand::Abort { error } => abort(state, error),
        OrchestrationCommand::Cancel => cancel(state),
        OrchestrationCommand::CancellationCompleted => cancellation_completed(state),
        OrchestrationCommand::Recover => recover(compiled, state),
    }
}
