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
//!   **journal** of the other steps its `context_access` lets it see.

use std::collections::HashMap;

use harness_core::orchestration::{
    AttemptStatus, CommandRun, ContextAccess, OrchestrationDefinition, OrchestrationNode,
    OrchestrationNodeId, OrchestrationNodeKind, OrchestrationRunState, OutputBinding, StepActions,
    StepAttempt, StepRun,
};
use harness_protocol::events::AgentEvent;
use serde_json::Value;

use crate::markdown_reply;

const MAX_FILES_CHANGED: usize = 60;
const MAX_FILES_READ: usize = 40;
const MAX_COMMANDS: usize = 30;
const MAX_RESEARCH: usize = 10;
/// How much of a step's hand-off the journal quotes. Steps whose output is
/// bound to the reader are not quoted at all: the reader has them in full.
const JOURNAL_HANDOFF_CHARS: usize = 1_500;
/// How much of its own previous result a retried step is shown.
const PREVIOUS_RESULT_CHARS: usize = 4_000;
/// The journal as a whole; the oldest entries go first when it is longer.
const MAX_JOURNAL_CHARS: usize = 9_000;

/// Builds an attempt's [`StepActions`] from the agent events it emits.
#[derive(Default)]
pub(crate) struct ActionRecorder {
    /// Tool calls requested and not yet completed, by call id.
    pending: HashMap<String, (String, Value)>,
    actions: StepActions,
}

impl ActionRecorder {
    pub(crate) fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::ToolCallRequested { call } => {
                self.pending.insert(
                    call.id.to_string(),
                    (call.name.clone(), call.arguments.clone()),
                );
            }
            AgentEvent::ToolCallCompleted { call_id, result } => {
                if let Some((name, arguments)) = self.pending.remove(&call_id.to_string()) {
                    self.record(&name, &arguments, result.has_error, &result.output_preview);
                }
            }
            _ => {}
        }
    }

    fn record(&mut self, name: &str, arguments: &Value, failed: bool, output: &str) {
        let actions = &mut self.actions;
        if failed {
            actions.tool_errors += 1;
        }
        match name {
            "write_file" | "edit_file" | "fs.edit" | "fs.write" if !failed => {
                if let Some(path) = path_of(arguments) {
                    push_unique(&mut actions.files_changed, path, MAX_FILES_CHANGED);
                }
            }
            "read_file" | "fs.read" | "open_document" if !failed => {
                if let Some(path) = path_of(arguments) {
                    push_unique(&mut actions.files_read, path, MAX_FILES_READ);
                }
            }
            "run_command" | "shell.exec" => {
                let command = match arguments.get("command") {
                    Some(Value::String(command)) => command.clone(),
                    Some(Value::Array(parts)) => parts
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => return,
                };
                // `run_command` reports a non-zero exit as text, not as an error.
                let failed = failed || exited_non_zero(output);
                push_command(actions, command, failed);
            }
            "run_check" => {
                let check = arguments
                    .get("check")
                    .or_else(|| arguments.get("checks"))
                    .map(|value| match value {
                        Value::String(check) => check.clone(),
                        Value::Array(checks) => checks
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", "),
                        _ => String::new(),
                    })
                    .filter(|check| !check.is_empty())
                    .unwrap_or_else(|| "project checks".into());
                push_command(actions, format!("check: {check}"), failed);
            }
            "install_dependencies" => push_command(actions, "install dependencies".into(), failed),
            "request_research" => {
                if let Some(question) = arguments.get("question").and_then(Value::as_str) {
                    if actions.research.len() < MAX_RESEARCH {
                        actions.research.push(question.trim().to_owned());
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn finish(self) -> StepActions {
        self.actions
    }
}

fn path_of(arguments: &Value) -> Option<String> {
    ["path", "file_path", "file"]
        .iter()
        .find_map(|key| arguments.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
        .filter(|path| !path.trim().is_empty())
}

fn push_unique(list: &mut Vec<String>, item: String, max: usize) {
    if list.len() < max && !list.contains(&item) {
        list.push(item);
    }
}

fn push_command(actions: &mut StepActions, command: String, failed: bool) {
    // Keep the latest: the last runs show where the work ended.
    if actions.commands.len() == MAX_COMMANDS {
        actions.commands.remove(0);
    }
    actions.commands.push(CommandRun { command, failed });
}

fn exited_non_zero(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    ["exit code", "exit status", "exited with"]
        .iter()
        .any(|marker| {
            lower.match_indices(marker).any(|(at, _)| {
                let rest = lower[at + marker.len()..].trim_start_matches([':', ' ', '=']);
                let code: String = rest.chars().take_while(char::is_ascii_digit).collect();
                !code.trim_start_matches('0').is_empty()
            })
        })
}

/// One line per kind of action, for prompts.
pub(crate) fn render_actions(actions: &StepActions) -> String {
    let mut lines = Vec::new();
    if !actions.files_changed.is_empty() {
        lines.push(format!("- Changed: {}", actions.files_changed.join(", ")));
    }
    if !actions.commands.is_empty() {
        let runs: Vec<String> = actions
            .commands
            .iter()
            .map(|run| {
                format!(
                    "`{}` ({})",
                    clip(&run.command, 120),
                    if run.failed { "failed" } else { "passed" }
                )
            })
            .collect();
        lines.push(format!("- Ran: {}", runs.join(", ")));
    }
    if !actions.research.is_empty() {
        lines.push(format!(
            "- Asked for research: {}",
            actions.research.join(" | ")
        ));
    }
    if !actions.files_read.is_empty() {
        lines.push(format!("- Read: {}", actions.files_read.join(", ")));
    }
    if actions.tool_errors > 0 {
        lines.push(format!("- Tool calls that failed: {}", actions.tool_errors));
    }
    lines.join("\n")
}

fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}\n[… shortened …]")
}

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

fn merge(into: &mut StepActions, from: &StepActions) {
    for file in &from.files_changed {
        push_unique(&mut into.files_changed, file.clone(), MAX_FILES_CHANGED);
    }
    for file in &from.files_read {
        push_unique(&mut into.files_read, file.clone(), MAX_FILES_READ);
    }
    for run in &from.commands {
        push_command(into, run.command.clone(), run.failed);
    }
    for question in &from.research {
        if into.research.len() < MAX_RESEARCH {
            into.research.push(question.clone());
        }
    }
    into.tool_errors += from.tool_errors;
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::{
        ids::ToolCallId,
        tools::{ToolCall, ToolResultSummary},
    };
    use serde_json::json;

    fn call(
        recorder: &mut ActionRecorder,
        name: &str,
        arguments: Value,
        failed: bool,
        output: &str,
    ) {
        let id = ToolCallId::new();
        recorder.observe(&AgentEvent::ToolCallRequested {
            call: ToolCall {
                id,
                name: name.into(),
                arguments,
            },
        });
        recorder.observe(&AgentEvent::ToolCallCompleted {
            call_id: id,
            result: ToolResultSummary {
                has_error: failed,
                output_preview: output.into(),
            },
        });
    }

    #[test]
    fn records_what_the_tool_calls_did() {
        let mut recorder = ActionRecorder::default();
        call(
            &mut recorder,
            "read_file",
            json!({"path": "src/a.rs"}),
            false,
            "",
        );
        call(
            &mut recorder,
            "edit_file",
            json!({"path": "src/a.rs"}),
            false,
            "",
        );
        call(
            &mut recorder,
            "write_file",
            json!({"path": "src/b.rs"}),
            true,
            "refused",
        );
        call(
            &mut recorder,
            "write_file",
            json!({"path": "src/a.rs"}),
            false,
            "",
        );
        call(
            &mut recorder,
            "run_command",
            json!({"command": "cargo test"}),
            false,
            "…\nExit code: 101",
        );
        call(
            &mut recorder,
            "run_command",
            json!({"command": ["ls", "-la"]}),
            false,
            "exit code: 0",
        );
        call(
            &mut recorder,
            "run_check",
            json!({"check": "typecheck"}),
            true,
            "",
        );
        call(
            &mut recorder,
            "request_research",
            json!({"question": "How is X configured?"}),
            false,
            "",
        );
        let actions = recorder.finish();
        assert_eq!(actions.files_changed, vec!["src/a.rs"]);
        assert_eq!(actions.files_read, vec!["src/a.rs"]);
        assert_eq!(
            actions.commands,
            vec![
                CommandRun {
                    command: "cargo test".into(),
                    failed: true
                },
                CommandRun {
                    command: "ls -la".into(),
                    failed: false
                },
                CommandRun {
                    command: "check: typecheck".into(),
                    failed: true
                },
            ]
        );
        assert_eq!(actions.research, vec!["How is X configured?"]);
        assert_eq!(actions.tool_errors, 2);
        let rendered = render_actions(&actions);
        assert!(rendered.contains("- Changed: src/a.rs"));
        assert!(rendered.contains("`cargo test` (failed)"));
    }

    #[test]
    fn a_retried_step_sees_what_its_last_attempt_did_and_handed_on() {
        let mut step = StepRun::default();
        assert!(previous_attempt(&step).is_none());
        let mut details = harness_core::orchestration::AttemptDetails::default();
        details.actions.files_changed.push("src/lib.rs".into());
        step.attempts.push(StepAttempt {
            attempt: 1,
            status: AttemptStatus::Succeeded,
            output: Some(json!("Implemented the parser fix.")),
            error: None,
            details,
        });
        step.attempts.push(StepAttempt {
            attempt: 2,
            status: AttemptStatus::Running,
            output: None,
            error: None,
            details: Default::default(),
        });
        let text = previous_attempt(&step).unwrap();
        assert!(text.starts_with("Attempt 1 finished"));
        assert!(text.contains("- Changed: src/lib.rs"));
        assert!(text.contains("Implemented the parser fix."));
    }
}
