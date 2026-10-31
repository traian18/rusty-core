//! Recording what an attempt did, from its tool calls rather than from what the model says it did.

use std::collections::HashMap;

use rusty_protocol::events::AgentEvent;
use rusty_workflow_core::{CommandRun, StepActions};
use serde_json::Value;

use super::render::clip;

pub(super) const MAX_FILES_CHANGED: usize = 60;

pub(super) const MAX_FILES_READ: usize = 40;

pub(super) const MAX_COMMANDS: usize = 30;

pub(super) const MAX_RESEARCH: usize = 10;

/// Research evidence lines kept per step, and how long each may be.
pub(super) const MAX_LOCATIONS: usize = 30;

pub(super) const MAX_LOCATION_CHARS: usize = 200;

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
            AgentEvent::ToolCallDenied { call_id, .. } => {
                self.pending.remove(&call_id.to_string());
            }
            _ => {}
        }
    }

    /// Whether a tool call is running, or waiting for the user to approve it: a
    /// command or check can take minutes without the attempt sending anything.
    pub(crate) fn calls_in_flight(&self) -> bool {
        !self.pending.is_empty()
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
            "run_command" | "shell.exec" => record_command(actions, arguments, failed, output),
            "run_check" => {
                let check = check_name(arguments);
                push_command(actions, format!("check: {check}"), failed);
            }
            "install_dependencies" => push_command(actions, "install dependencies".into(), failed),
            "request_research" => record_research(actions, arguments, failed, output),
            _ => {}
        }
    }

    pub(crate) fn finish(self) -> StepActions {
        self.actions
    }
}

/// A shell command; the arguments may spell it as one string or as its parts.
fn record_command(actions: &mut StepActions, arguments: &Value, failed: bool, output: &str) {
    let command = match arguments.get("command") {
        Some(Value::String(command)) => command.clone(),
        Some(Value::Array(parts)) => joined(parts, " "),
        _ => return,
    };
    // `run_command` reports a non-zero exit as text, not as an error.
    let failed = failed || exited_non_zero(output);
    push_command(actions, command, failed);
}

/// The check a `run_check` call asked for, as a short label.
fn check_name(arguments: &Value) -> String {
    let named = arguments
        .get("check")
        .or_else(|| arguments.get("checks"))
        .map(|value| match value {
            Value::String(check) => check.clone(),
            Value::Array(checks) => joined(checks, ", "),
            _ => String::new(),
        })
        .filter(|check| !check.is_empty());
    named.unwrap_or_else(|| "project checks".into())
}

fn joined(parts: &[Value], separator: &str) -> String {
    parts
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(separator)
}

/// A question asked of a research step, and the places its answer pointed to.
fn record_research(actions: &mut StepActions, arguments: &Value, failed: bool, output: &str) {
    if let Some(question) = arguments.get("question").and_then(Value::as_str) {
        if actions.research.len() < MAX_RESEARCH {
            actions.research.push(question.trim().to_owned());
        }
    }
    if !failed {
        for location in evidence_locations(output) {
            push_unique(&mut actions.locations, location, MAX_LOCATIONS);
        }
    }
}

/// The `## Evidence` lines of a research answer that name a place in the
/// project (`path:lines`, not a URL), as the research step was told to write them.
pub(super) fn evidence_locations(output: &str) -> Vec<String> {
    // The research step's reply reaches the tool result as `{"findings": "..."}`.
    let findings = serde_json::from_str::<Value>(output)
        .ok()
        .and_then(|value| {
            value
                .get("findings")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| output.to_owned());
    let mut in_evidence = false;
    let mut found = Vec::new();
    for line in findings.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            in_evidence = trimmed.eq_ignore_ascii_case("## Evidence");
            continue;
        }
        if in_evidence
            && trimmed
                .split([' ', '`', ',', ';', '(', ')'])
                .any(is_file_location)
        {
            let item = trimmed
                .trim_start_matches(['-', '*', ' '])
                .replace("**", "");
            found.push(clip(item.trim(), MAX_LOCATION_CHARS));
        }
    }
    found
}

/// `path/to/file.ext:12` or `file.ext:12-30`: a file with an extension, then line numbers.
pub(super) fn is_file_location(token: &str) -> bool {
    let Some((path, lines)) = token.trim_end_matches(['.', ':']).split_once(':') else {
        return false;
    };
    let file = path.rsplit('/').next().unwrap_or(path);
    !path.contains("//")
        && file.contains('.')
        && !file.starts_with('.')
        && lines.starts_with(|c: char| c.is_ascii_digit())
        && lines
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | ',' | '–'))
}

pub(super) fn path_of(arguments: &Value) -> Option<String> {
    ["path", "file_path", "file"]
        .iter()
        .find_map(|key| arguments.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
        .filter(|path| !path.trim().is_empty())
}

pub(super) fn push_unique(list: &mut Vec<String>, item: String, max: usize) {
    if list.len() < max && !list.contains(&item) {
        list.push(item);
    }
}

pub(super) fn push_command(actions: &mut StepActions, command: String, failed: bool) {
    // Keep the latest: the last runs show where the work ended.
    if actions.commands.len() == MAX_COMMANDS {
        actions.commands.remove(0);
    }
    actions.commands.push(CommandRun { command, failed });
}

pub(super) fn exited_non_zero(output: &str) -> bool {
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

pub(super) fn merge(into: &mut StepActions, from: &StepActions) {
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
    for location in &from.locations {
        push_unique(&mut into.locations, location.clone(), MAX_LOCATIONS);
    }
    into.tool_errors += from.tool_errors;
}
