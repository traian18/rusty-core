//! Answering caller requests: pause, resume, permission decisions, answers.

use harness_core::orchestration::{InputResponse, OrchestrationCommand};
use harness_protocol::commands::PermissionDecision;
use tokio::{sync::oneshot, time::Instant};

use super::super::super::steps::{InputAnswer, PermissionResolution};
use super::super::{
    handle::{Control, ControlRequest},
    ControlError, OrchestrationRuntimeError,
};
use super::{ApplyError, RunLoop};

/// The caller's outcome, or a fatal error that also ends the run.
type ControlOutcome = Result<Result<(), ControlError>, OrchestrationRuntimeError>;

impl RunLoop {
    pub(super) async fn on_control(
        &mut self,
        control: Control,
    ) -> Result<(), OrchestrationRuntimeError> {
        // Pausing, resuming and answering a permission all restart the stall
        // clock, which does not run while waiting on a person.
        if let Some(active) = self.active.as_mut() {
            active.last_activity = Instant::now();
        }
        let Control { request, reply } = control;
        let outcome = match request {
            ControlRequest::Pause => self.apply_control(OrchestrationCommand::Pause).await,
            ControlRequest::Resume => self.apply_control(OrchestrationCommand::Resume).await,
            ControlRequest::ResolvePermission {
                permission_id,
                decision,
            } => self.resolve_permission(permission_id, decision).await,
            ControlRequest::ResolveInput {
                request_id,
                response,
            } => self.resolve_input(request_id, response).await,
        };
        match outcome {
            Ok(outcome) => {
                send_reply(reply, outcome);
                Ok(())
            }
            Err(fatal) => {
                send_reply(reply, Err(ControlError::Rejected(fatal.to_string())));
                Err(fatal)
            }
        }
    }

    async fn apply_control(&mut self, command: OrchestrationCommand) -> ControlOutcome {
        match self.try_apply(command).await {
            Ok(()) => Ok(Ok(())),
            Err(ApplyError::Rejected(error)) => Ok(Err(ControlError::Rejected(error.message))),
            Err(ApplyError::Fatal(error)) => Err(error),
        }
    }

    async fn resolve_permission(
        &mut self,
        permission_id: String,
        decision: PermissionDecision,
    ) -> ControlOutcome {
        match self.deliver_permission(permission_id, decision) {
            Ok(command) => self.apply_control(command).await,
            Err(rejected) => Ok(Err(rejected)),
        }
    }

    /// Hands the decision to the waiting attempt and returns the command that
    /// records it.
    fn deliver_permission(
        &self,
        permission_id: String,
        decision: PermissionDecision,
    ) -> Result<OrchestrationCommand, ControlError> {
        let waiting = self.active.as_ref().filter(|active| {
            self.state.steps[&active.node_id]
                .pending_permissions
                .contains(&permission_id)
        });
        let Some(active) = waiting else {
            return Err(ControlError::Rejected(format!(
                "no active step is waiting on permission {permission_id}"
            )));
        };
        // Deliver to the delegated agent first: if the attempt already
        // ended, the decision has nowhere to go.
        let approved = matches!(decision, PermissionDecision::Approved);
        active
            .permissions
            .send(PermissionResolution {
                permission_id: permission_id.clone(),
                decision,
            })
            .map_err(|_| {
                ControlError::Rejected("the attempt awaiting this permission has ended".into())
            })?;
        Ok(OrchestrationCommand::PermissionResolved {
            node_id: active.node_id.clone(),
            attempt: active.attempt,
            permission_id,
            approved,
        })
    }

    async fn resolve_input(
        &mut self,
        request_id: String,
        response: InputResponse,
    ) -> ControlOutcome {
        let Some(active) = &self.active else {
            return Ok(Err(ControlError::Rejected(format!(
                "no active step is waiting on question {request_id}"
            ))));
        };
        // The reducer checks the answer is one that was offered before the
        // waiting step acts on it.
        let command = OrchestrationCommand::InputResolved {
            node_id: active.node_id.clone(),
            attempt: active.attempt,
            request_id: request_id.clone(),
            response: response.clone(),
        };
        if let Err(rejected) = self.apply_control(command).await? {
            return Ok(Err(rejected));
        }
        Ok(self.deliver_answer(InputAnswer {
            request_id,
            response,
        }))
    }

    fn deliver_answer(&self, answer: InputAnswer) -> Result<(), ControlError> {
        let delivered = self
            .active
            .as_ref()
            .is_some_and(|active| active.answers.send(answer).is_ok());
        if delivered {
            Ok(())
        } else {
            Err(ControlError::Rejected(
                "the attempt awaiting this answer has ended".into(),
            ))
        }
    }
}

/// The caller may have dropped its handle while waiting; then nobody needs
/// the reply and there is nothing more to do.
fn send_reply(reply: oneshot::Sender<Result<(), ControlError>>, outcome: Result<(), ControlError>) {
    let _ = reply.send(outcome);
}
