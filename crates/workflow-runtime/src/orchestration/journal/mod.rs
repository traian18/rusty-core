//! What a workflow step is told about the rest of its run.
//!
//! Every step runs in a fresh session, so without this a builder sent back by
//! its verifier starts over knowing only why it was rejected, and every step
//! rediscovers what the ones before it already found. Two things fix that:
//!
//! * each attempt's **actions** (files changed and read, commands and checks
//!   run, research asked for) are recorded from its tool calls, not from what
//!   the model says it did;
//! * a step is shown its own **previous attempt** when it runs again, and the
//!
//! `actions` records an attempt's actions, `render` writes them as text; this file assembles the
//! previous-attempt and journal sections a step is shown.

mod actions;
mod render;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use rusty_workflow_core::{
    AttemptStatus, ContextAccess, OrchestrationDefinition, OrchestrationNode, OrchestrationNodeId,
    OrchestrationNodeKind, OrchestrationRunState, OutputBinding, StepActions, StepAttempt, StepRun,
};

use crate::markdown_reply;

use actions::merge;
pub(crate) use actions::ActionRecorder;
use render::clip;
pub(crate) use render::render_actions;

/// How much of a step's hand-off the journal quotes. Steps whose output is
/// bound to the reader are not quoted at all: the reader has them in full.
const JOURNAL_HANDOFF_CHARS: usize = 1_500;

/// How much of its own previous result a retried step is shown.
const PREVIOUS_RESULT_CHARS: usize = 4_000;

/// The journal as a whole; the oldest entries go first when it is longer.
const MAX_JOURNAL_CHARS: usize = 9_000;

fn finished(step: &StepRun) -> impl DoubleEndedIterator<Item = &StepAttempt> {
    step.attempts
        .iter()
        .filter(|attempt| attempt.status != AttemptStatus::Running)
}

/// What the step's latest finished attempt did, for the step running again.
/// `None` on a first attempt.
pub(crate) fn previous_attempt(step: &StepRun) -> Option<String> {
    let attempt = finished(step).next_back()?;
    let mut text = format!(
        "Attempt {} {}.",
        attempt.attempt,
        match (&attempt.status, &attempt.error) {
            (AttemptStatus::Succeeded, _) => "finished and handed its result on".to_owned(),
            (AttemptStatus::Cancelled, _) => "was cancelled".to_owned(),
            (_, Some(error)) => format!("failed: [{}] {}", error.code, clip(&error.message, 600)),
            _ => "failed".to_owned(),
        }
    );
    let actions = render_actions(&attempt.details.actions);
    if !actions.is_empty() {
        text.push_str("\nWhat it did:\n");
        text.push_str(&actions);
    }
    if let Some(output) = &attempt.output {
        text.push_str("\nIts result:\n");
        text.push_str(&clip(
            &markdown_reply::render_input(output),
            PREVIOUS_RESULT_CHARS,
        ));
    }
    Some(text)
}

/// The records of the steps `access` lets `node` see, oldest first. `None`
/// when there is nothing to show.
pub(crate) fn journal(
    definition: &OrchestrationDefinition,
    state: &OrchestrationRunState,
    node: &OrchestrationNode,
    access: &ContextAccess,
) -> Option<String> {
    if access.is_own() {
        return None;
    }
    // Outputs bound to this step are already in its input, in full.
    let bound: HashMap<&OrchestrationNodeId, &str> = node
        .input_bindings
        .iter()
        .filter_map(|binding| match &binding.source {
            OutputBinding::NodeOutput { node_id, .. } => Some((node_id, binding.target.as_str())),
            OutputBinding::RunInput { .. } => None,
        })
        .collect();
    let mut entries: Vec<(u64, String)> = definition
        .nodes
        .iter()
        .filter(|other| other.id != node.id && access.includes(&other.id))
        .filter_map(|other| {
            let step = state.steps.get(&other.id)?;
            let started = finished(step)
                .next()
                .and_then(|attempt| attempt.details.started_at_ms)?;
            entry(other, step, bound.get(&other.id).copied()).map(|text| (started, text))
        })
        .collect();
    if entries.is_empty() {
        return None;
    }
    entries.sort_by_key(|(started, _)| *started);
    let mut texts: Vec<String> = entries.into_iter().map(|(_, text)| text).collect();
    let mut dropped = 0;
    while texts.len() > 1 && texts.iter().map(String::len).sum::<usize>() > MAX_JOURNAL_CHARS {
        texts.remove(0);
        dropped += 1;
    }
    let mut journal = String::new();
    if dropped > 0 {
        journal.push_str(&format!(
            "({dropped} earlier step(s) left out for length.)\n\n"
        ));
    }
    journal.push_str(&texts.join("\n\n"));
    Some(journal)
}

fn entry(node: &OrchestrationNode, step: &StepRun, bound_as: Option<&str>) -> Option<String> {
    let latest = finished(step).next_back()?;
    let attempts = finished(step).count();
    let status = match latest.status {
        AttemptStatus::Succeeded => "done",
        AttemptStatus::Cancelled => "cancelled",
        _ => "failed",
    };
    let mut text = format!(
        "### {} ({status}{})",
        node.name,
        if attempts > 1 {
            format!(", {attempts} attempts")
        } else {
            String::new()
        }
    );
    match &node.kind {
        OrchestrationNodeKind::Agent(_) | OrchestrationNodeKind::Subflow(_) => {
            // Everything every attempt changed is still in the workspace.
            let mut actions = StepActions::default();
            for attempt in finished(step) {
                merge(&mut actions, &attempt.details.actions);
            }
            let rendered = render_actions(&actions);
            if !rendered.is_empty() {
                text.push('\n');
                text.push_str(&rendered);
            }
            if let Some(error) = latest.error.as_ref().filter(|_| status == "failed") {
                text.push_str(&format!("\n- Last failure: {}", clip(&error.message, 400)));
            }
            match (bound_as, &step.output) {
                (Some(name), Some(_)) => {
                    text.push_str(&format!("\n- Hand-off: given to you as `{name}`."))
                }
                (None, Some(output)) => {
                    text.push_str("\n- Hand-off:\n");
                    text.push_str(&clip(
                        &markdown_reply::render_input(output),
                        JOURNAL_HANDOFF_CHARS,
                    ));
                }
                _ => {}
            }
        }
        OrchestrationNodeKind::Verify(_) => {
            if let Some(error) = &latest.error {
                text.push_str(&format!("\n- Rejected: {}", clip(&error.message, 800)));
            } else {
                text.push_str("\n- Passed.");
            }
        }
        OrchestrationNodeKind::Approval(_) => {
            if let Some(error) = &latest.error {
                text.push_str(&format!(
                    "\n- The user asked for: {}",
                    clip(&error.message, 800)
                ));
            } else {
                text.push_str("\n- Approved.");
            }
        }
        OrchestrationNodeKind::Input(_) | OrchestrationNodeKind::Output(_) => return None,
    }
    Some(text)
}
