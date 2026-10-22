//! The `approval` node: a person approves the subject, asks for changes or
//! rejects it.

use harness_core::orchestration::{
    ApprovalNodeConfig, AttemptStatus, InputDecision, InputRequest, InputResponse,
    OrchestrationError, OrchestrationNode, OrchestrationRunState, Responder, RetryReason,
};
use serde_json::{json, Value};

use super::{resolve_binding, StepContext};

/// Feedback code for changes a user asked for at an approval step. Its
/// message is the user's own request, not model output.
pub const CHANGES_REQUESTED: &str = "changes_requested";

/// Feedback code for notes a user gave while approving, at a step that
/// revises on notes: the changes are made like requested ones, and the
/// approval then stands without asking again.
pub const CHANGES_APPROVED: &str = "changes_approved";

/// Whether feedback with this code is the user's own request for changes
/// (an instruction to the revised step), not a rejection reason.
pub fn is_user_change(code: &str) -> bool {
    code == CHANGES_REQUESTED || code == CHANGES_APPROVED
}

/// The approval step: asks the user to approve `subject`, ask for changes or
/// reject it, unless it is skipped (nothing to review) or the run was
/// started with auto-approve. Requested changes fail the step with
/// `changes_requested`, which the reducer routes to the revise target.
pub(crate) async fn execute_approval(
    state: &OrchestrationRunState,
    node: &OrchestrationNode,
    config: &ApprovalNodeConfig,
    attempt: u32,
    context: &mut StepContext,
) -> Result<Value, OrchestrationError> {
    let subject = approval_subject(state, config)?;
    if let Some(outcome) = decide_without_asking(state, node, config, &subject) {
        return Ok(outcome);
    }
    let response = context
        .ask(InputRequest {
            id: format!("{}:{attempt}", node.id),
            kind: "approval".into(),
            prompt: approval_prompt(node, config),
            subject: subject.clone(),
            decisions: approval_decisions(config, attempt),
        })
        .await?;
    answer(response, node, config, attempt, &subject)
}

fn approval_subject(
    state: &OrchestrationRunState,
    config: &ApprovalNodeConfig,
) -> Result<Value, OrchestrationError> {
    match resolve_binding(state, &config.subject) {
        // Nothing where the subject should be is nothing to review.
        Err(error) if error.code == "invalid_pointer" && config.skip_if_empty.is_some() => {
            Ok(Value::Null)
        }
        result => result,
    }
}

fn outcome(decision: &str, by: Responder, notes: Option<String>, subject: &Value) -> Value {
    json!({
        "decision": decision,
        "by": by,
        "notes": notes.unwrap_or_default(),
        "subject": subject,
    })
}

/// The outcome when nobody needs to be asked: nothing to review, an earlier
/// approval that still stands, or a run that approves automatically.
fn decide_without_asking(
    state: &OrchestrationRunState,
    node: &OrchestrationNode,
    config: &ApprovalNodeConfig,
    subject: &Value,
) -> Option<Value> {
    let nothing_to_review = config
        .skip_if_empty
        .as_ref()
        .is_some_and(|pointer| is_empty_at(subject, pointer));
    if nothing_to_review {
        return Some(outcome("skipped", Responder::Auto, None, subject));
    }
    if let Some(notes) = standing_approval_notes(state, node) {
        return Some(outcome("approved", Responder::User, Some(notes), subject));
    }
    if state.options.auto_approve && config.allow_auto_approve {
        return Some(outcome("approved", Responder::Auto, None, subject));
    }
    None
}

fn is_empty_at(subject: &Value, pointer: &str) -> bool {
    match subject.pointer(pointer) {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => text.trim().is_empty(),
        Some(Value::Array(items)) => items.is_empty(),
        Some(Value::Object(fields)) => fields.is_empty(),
        Some(_) => false,
    }
}

/// The user approved the previous result with notes, and the subject has
/// since been revised to include them: that approval stands.
fn standing_approval_notes(
    state: &OrchestrationRunState,
    node: &OrchestrationNode,
) -> Option<String> {
    let step = state.steps.get(&node.id)?;
    step.attempts
        .iter()
        .rev()
        .find(|attempt| attempt.status != AttemptStatus::Running)
        .and_then(|attempt| attempt.error.as_ref())
        .filter(|error| error.code == CHANGES_APPROVED)
        .map(|error| error.message.clone())
}

/// Whether this attempt may still send the subject back for revision.
fn can_revise(config: &ApprovalNodeConfig, attempt: u32) -> bool {
    config.revise_target().is_some() && attempt <= config.max_revisions
}

fn approval_decisions(config: &ApprovalNodeConfig, attempt: u32) -> Vec<InputDecision> {
    let decision = |id: &str, label: &str, requires_text: bool| InputDecision {
        id: id.into(),
        label: label.into(),
        requires_text,
    };
    let mut decisions = vec![decision("approve", "Approve", false)];
    if can_revise(config, attempt) {
        decisions.push(decision("request_changes", "Request changes", true));
    }
    decisions.push(decision("reject", "Reject", false));
    decisions
}

fn approval_prompt(node: &OrchestrationNode, config: &ApprovalNodeConfig) -> String {
    if config.prompt.trim().is_empty() {
        format!("Review {} before the workflow continues.", node.name)
    } else {
        config.prompt.clone()
    }
}

/// Turn the user's answer into the step's outcome.
fn answer(
    response: InputResponse,
    node: &OrchestrationNode,
    config: &ApprovalNodeConfig,
    attempt: u32,
    subject: &Value,
) -> Result<Value, OrchestrationError> {
    let notes = response.text.clone().filter(|text| !text.trim().is_empty());
    let revise_on_notes = config.revise_on_notes && notes.is_some() && can_revise(config, attempt);
    match response.decision.as_str() {
        "approve" if revise_on_notes => Err(OrchestrationError::retryable(
            CHANGES_APPROVED,
            notes.unwrap_or_default(),
            RetryReason::ChangesRequested,
        )),
        "approve" => Ok(outcome("approved", response.by, notes, subject)),
        "request_changes" => Err(OrchestrationError::retryable(
            CHANGES_REQUESTED,
            notes.unwrap_or_default(),
            RetryReason::ChangesRequested,
        )),
        _ => Err(OrchestrationError::new(
            "approval_rejected",
            match notes {
                Some(notes) => format!("{} was rejected: {notes}", node.name),
                None => format!("{} was rejected.", node.name),
            },
        )),
    }
}
