//! Driving the child run of a `subflow` node from inside the parent's step.
//!
//! The child is an ordinary run with its own state and events. What the
//! parent step does is lend it the parent's channel to the outside: the
//! child's tool permission requests, its questions to the user and its agent
//! events surface as the parent step's, and the parent's cancellation and the
//! answers it receives go back to the child.

use harness_core::orchestration::{
    InputRequest, OrchestrationEvent, OrchestrationOutcome, OrchestrationResult,
};
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;

use super::{
    runner::{OrchestrationHandle, OrchestrationUpdate},
    steps::{AgentExecutionError, StepContext, StepSignal},
};

/// Runs `child` to its end on behalf of the step `context` belongs to, and
/// returns its output. `label` names the child in the questions it asks.
pub async fn drive_child(
    mut child: OrchestrationHandle,
    context: &mut StepContext,
    label: &str,
) -> Result<Value, AgentExecutionError> {
    let mut updates = child.subscribe();
    let mut state = child.watch();
    let mut cancelled = false;
    loop {
        if state.borrow().status.is_terminal() {
            break;
        }
        tokio::select! {
            _ = context.cancellation.cancelled(), if !cancelled => {
                cancelled = true;
                child.cancel();
            }
            permission = context.permissions.recv() => {
                if let Some(permission) = permission {
                    // A refused delivery means the request was already settled.
                    let _ = child
                        .resolve_permission(permission.permission_id, permission.decision)
                        .await;
                }
            }
            update = updates.recv() => match update {
                Ok(OrchestrationUpdate::Agent(correlated)) => {
                    context.signal(StepSignal::Agent(Box::new(correlated.envelope)));
                }
                Ok(OrchestrationUpdate::Event(envelope)) => match envelope.event {
                    OrchestrationEvent::PermissionRequested { permission_id, .. } => {
                        context.signal(StepSignal::PermissionRequested {
                            permission_id,
                            tool_name: String::new(),
                        });
                    }
                    OrchestrationEvent::InputRequested { request, .. } => {
                        if let Err(error) = relay_question(&child, context, label, *request).await {
                            child.cancel();
                            return Err(error);
                        }
                    }
                    _ => {}
                },
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            },
            changed = state.changed() => {
                if changed.is_err() {
                    break;
                }
            }
        }
    }
    let output = child
        .wait()
        .await
        .map_err(|error| AgentExecutionError::new("subflow_failed", error.to_string()))?;
    context.signal(StepSignal::Usage(output.result.usage.clone()));
    outcome(output.result)
}

/// Asks the user on the parent's behalf and hands the answer to the child.
async fn relay_question(
    child: &OrchestrationHandle,
    context: &mut StepContext,
    label: &str,
    request: InputRequest,
) -> Result<(), AgentExecutionError> {
    let child_id = request.id.clone();
    let response = context
        .ask(InputRequest {
            id: format!("{label}/{child_id}"),
            ..request
        })
        .await?;
    child
        .resolve_input(child_id, response)
        .await
        .map_err(|error| AgentExecutionError::new("subflow_failed", error.to_string()))
}

fn outcome(result: OrchestrationResult) -> Result<Value, AgentExecutionError> {
    match result.status {
        OrchestrationOutcome::Completed => Ok(result.output.unwrap_or(Value::Null)),
        OrchestrationOutcome::Cancelled => Err(AgentExecutionError::new(
            "cancelled",
            "the flow was cancelled",
        )),
        OrchestrationOutcome::Failed => {
            let error = result.error;
            Err(AgentExecutionError::new(
                "subflow_failed",
                format!(
                    "{} ({})",
                    error
                        .as_ref()
                        .map_or("the flow failed", |e| e.message.as_str()),
                    error.as_ref().map_or("unknown", |e| e.code.as_str())
                ),
            ))
        }
    }
}
