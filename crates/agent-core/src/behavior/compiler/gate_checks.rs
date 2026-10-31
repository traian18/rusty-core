//! Checks on the completion gate: its checks, their conditions, and their evaluators.

use std::collections::BTreeSet;

use super::{condition_checks::check_condition, Issues};
use crate::behavior::definition::{CompletionGate, Condition, EvaluatorSpec, GateCheck, RuleEvent};

/// The most transcript messages an evaluator may be shown.
const MAX_TRANSCRIPT_MESSAGES: u32 = 200;

pub(super) fn check_gate(gate: &CompletionGate, issues: &mut Issues) {
    if gate.checks.is_empty() {
        issues.add(
            "completion_gate.checks",
            "empty_gate",
            "a completion gate needs at least one check",
        );
    }
    let mut ids = BTreeSet::new();
    for (index, check) in gate.checks.iter().enumerate() {
        let path = format!("completion_gate.checks[{index}]");
        check_check_id(check, &path, &mut ids, issues);
        check_check_body(check, &path, issues);
    }
}

fn check_check_id<'a>(
    check: &'a GateCheck,
    path: &str,
    ids: &mut BTreeSet<&'a str>,
    issues: &mut Issues,
) {
    if check.id.trim().is_empty() {
        issues.add(
            &format!("{path}.id"),
            "empty_id",
            "check id cannot be empty",
        );
    } else if !ids.insert(check.id.as_str()) {
        issues.add(
            &format!("{path}.id"),
            "duplicate_check_id",
            format!("check id {} is used twice", check.id),
        );
    }
}

/// A check is either a `require` condition with feedback, or an `evaluator`; never both or neither.
fn check_check_body(check: &GateCheck, path: &str, issues: &mut Issues) {
    match (&check.require, &check.evaluator) {
        (Some(condition), None) => {
            check_gate_condition(condition, &format!("{path}.require"), issues);
            let no_feedback = check
                .feedback
                .as_deref()
                .is_none_or(|text| text.trim().is_empty());
            if no_feedback {
                issues.add(
                    &format!("{path}.feedback"),
                    "missing_feedback",
                    "a `require` check needs feedback to send back when it fails",
                );
            }
        }
        (None, Some(evaluator)) => {
            check_evaluator(evaluator, &format!("{path}.evaluator"), issues);
        }
        _ => issues.add(
            path,
            "invalid_check",
            "a check needs exactly one of `require` or `evaluator`",
        ),
    }
}

/// Gate conditions run when the model proposes to finish, so there is no tool call to inspect.
fn check_gate_condition(condition: &Condition, path: &str, issues: &mut Issues) {
    match condition {
        Condition::Tool(_)
        | Condition::Arg(_)
        | Condition::ResultContains(_)
        | Condition::RepeatedCall(_)
        | Condition::ProfileEnteredFrom(_) => issues.add(
            path,
            "condition_not_available",
            "tool, arg, result_contains and repeated_call are not available in a completion gate",
        ),
        Condition::All(conditions) | Condition::Any(conditions) => {
            for (index, condition) in conditions.iter().enumerate() {
                check_gate_condition(condition, &format!("{path}[{index}]"), issues);
            }
        }
        Condition::Not(inner) => check_gate_condition(inner, &format!("{path}.not"), issues),
        Condition::Turn(_)
        | Condition::Calls(_)
        | Condition::TurnsSinceCall(_)
        | Condition::SinceLastCall(_)
        | Condition::ToolOffered(_)
        | Condition::FinalStatus(_) => {}
    }
    check_condition(condition, RuleEvent::BeforeModelRequest, path, issues);
}

fn check_evaluator(evaluator: &EvaluatorSpec, path: &str, issues: &mut Issues) {
    match evaluator {
        EvaluatorSpec::Tool { tool, .. } => {
            if tool.trim().is_empty() {
                issues.add(
                    path,
                    "invalid_tool_reference",
                    "evaluator tool cannot be empty",
                );
            }
        }
        EvaluatorSpec::Model {
            instructions,
            model,
            transcript_messages,
        } => {
            issues.require_text(
                path,
                instructions,
                "model evaluator instructions cannot be empty",
            );
            check_model_name(model.as_deref(), path, issues);
            check_transcript_limit(*transcript_messages, path, issues);
        }
        EvaluatorSpec::Agent {
            instructions,
            tools,
            model,
            max_turns,
            transcript_messages,
        } => {
            issues.require_text(
                path,
                instructions,
                "agent evaluator instructions cannot be empty",
            );
            if tools.iter().any(|tool| tool.trim().is_empty()) {
                issues.add(path, "invalid_tool_reference", "tool ids cannot be empty");
            }
            check_model_name(model.as_deref(), path, issues);
            if !(1..=50).contains(max_turns) {
                issues.add(path, "invalid_limit", "max_turns must be between 1 and 50");
            }
            check_transcript_limit(*transcript_messages, path, issues);
        }
        EvaluatorSpec::Command {
            command,
            timeout_ms,
        } => {
            issues.require_text(path, command, "command cannot be empty");
            if !(1..=600_000).contains(timeout_ms) {
                issues.add(
                    path,
                    "invalid_limit",
                    "timeout_ms must be between 1 and 600000",
                );
            }
        }
    }
}

fn check_model_name(model: Option<&str>, path: &str, issues: &mut Issues) {
    if model.is_some_and(|model| model.trim().is_empty()) {
        issues.add(path, "empty_model", "model cannot be empty");
    }
}

fn check_transcript_limit(transcript_messages: u32, path: &str, issues: &mut Issues) {
    if transcript_messages > MAX_TRANSCRIPT_MESSAGES {
        issues.add(
            path,
            "invalid_limit",
            "transcript_messages must be at most 200",
        );
    }
}
