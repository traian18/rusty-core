//! Checks on rule conditions: which events a condition makes sense on, and that its arguments are
//! not empty.

use super::Issues;
use crate::behavior::definition::{ArgMatch, Comparison, Condition, NameMatch, RuleEvent};

pub(super) fn check_condition(
    condition: &Condition,
    event: RuleEvent,
    path: &str,
    issues: &mut Issues,
) {
    match condition {
        Condition::Tool(names) => {
            require_tool_event(event, "tool", path, issues);
            check_names(names, &format!("{path}.tool"), issues);
        }
        Condition::Arg(arg) => {
            require_tool_event(event, "arg", path, issues);
            check_arg(arg, path, issues);
        }
        Condition::ResultContains(_) => require_post_tool_event(event, path, issues),
        Condition::RepeatedCall(comparison) => {
            require_event(
                event,
                RuleEvent::PreToolUse,
                "repeated_call needs PreToolUse",
                path,
                issues,
            );
            check_comparison(comparison, &format!("{path}.repeated_call"), issues);
        }
        Condition::Turn(comparison) => {
            check_comparison(comparison, &format!("{path}.turn"), issues);
        }
        Condition::ToolOffered(names) => {
            check_names(names, &format!("{path}.tool_offered"), issues);
        }
        Condition::Calls(count) | Condition::TurnsSinceCall(count) => {
            check_names(&count.tool, &format!("{path}.tool"), issues);
            check_comparison(&count.comparison(), path, issues);
        }
        Condition::SinceLastCall(since) => {
            check_names(&since.of, &format!("{path}.since_last_call.of"), issues);
            check_names(
                &since.called,
                &format!("{path}.since_last_call.called"),
                issues,
            );
            check_comparison(
                &since.comparison(),
                &format!("{path}.since_last_call"),
                issues,
            );
        }
        Condition::FinalStatus(names) => check_final_status(names, path, issues),
        Condition::All(conditions) | Condition::Any(conditions) => {
            check_combinator(conditions, event, path, issues);
        }
        Condition::Not(inner) => check_condition(inner, event, &format!("{path}.not"), issues),
        Condition::ProfileEnteredFrom(names) => {
            require_event(
                event,
                RuleEvent::ProfileEntered,
                "profile_entered_from needs ProfileEntered",
                path,
                issues,
            );
            check_names(names, &format!("{path}.profile_entered_from"), issues);
        }
    }
}

/// Tool patterns must name something.
pub(super) fn check_names(names: &NameMatch, path: &str, issues: &mut Issues) {
    if names.patterns().is_empty() || names.patterns().iter().any(|p| p.trim().is_empty()) {
        issues.add(
            path,
            "invalid_tool_reference",
            "tool patterns cannot be empty",
        );
    }
}

fn check_comparison(comparison: &Comparison, path: &str, issues: &mut Issues) {
    if comparison.is_empty() {
        issues.add(
            path,
            "empty_comparison",
            "give at least one of eq, gte, lte",
        );
    }
}

fn check_arg(arg: &ArgMatch, path: &str, issues: &mut Issues) {
    if !arg.pointer.is_empty() && !arg.pointer.starts_with('/') {
        issues.add(
            &format!("{path}.arg.pointer"),
            "invalid_pointer",
            "use JSON Pointer syntax: empty or starting with '/'",
        );
    }
    if arg.glob.is_none() && arg.contains.is_none() && arg.equals.is_none() {
        issues.add(
            &format!("{path}.arg"),
            "empty_match",
            "give at least one of glob, contains, equals",
        );
    }
}

fn check_final_status(names: &NameMatch, path: &str, issues: &mut Issues) {
    if names.patterns().is_empty() || names.patterns().iter().any(|p| p.trim().is_empty()) {
        issues.add(
            &format!("{path}.final_status"),
            "empty_match",
            "final_status patterns cannot be empty",
        );
    }
}

fn check_combinator(conditions: &[Condition], event: RuleEvent, path: &str, issues: &mut Issues) {
    if conditions.is_empty() {
        issues.add(
            path,
            "empty_combinator",
            "all/any need at least one condition",
        );
    }
    for (index, condition) in conditions.iter().enumerate() {
        check_condition(condition, event, &format!("{path}[{index}]"), issues);
    }
}

fn require_tool_event(event: RuleEvent, what: &str, path: &str, issues: &mut Issues) {
    if !event.is_tool_event() {
        issues.add(
            path,
            "condition_not_available",
            format!("{what} needs a tool event, not {}", event.name()),
        );
    }
}

fn require_post_tool_event(event: RuleEvent, path: &str, issues: &mut Issues) {
    let after_tool = matches!(
        event,
        RuleEvent::PostToolUse | RuleEvent::PostToolUseFailure
    );
    if !after_tool {
        issues.add(
            path,
            "condition_not_available",
            format!(
                "result_contains needs a post-tool event, not {}",
                event.name()
            ),
        );
    }
}

/// `needs` is the message prefix, such as "repeated_call needs PreToolUse".
fn require_event(
    event: RuleEvent,
    wanted: RuleEvent,
    needs: &str,
    path: &str,
    issues: &mut Issues,
) {
    if event != wanted {
        issues.add(
            path,
            "condition_not_available",
            format!("{needs}, not {}", event.name()),
        );
    }
}
