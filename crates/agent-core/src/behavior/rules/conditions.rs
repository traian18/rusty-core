//! Whether a rule condition holds, and the pattern matching it relies on.

use serde_json::Value;

use super::ToolEvent;
use crate::{
    behavior::{
        definition::{ArgMatch, Condition, NameMatch},
        state::RunCounters,
    },
    tool_alias::followed_tool,
};

pub(super) fn holds(condition: &Condition, tool: Option<ToolEvent<'_>>, run: &RunCounters) -> bool {
    match condition {
        Condition::Tool(names) => {
            tool.is_some_and(|tool| tool_name_matches(names, &tool.call.name))
        }
        Condition::Arg(arg) => tool.is_some_and(|tool| arg_matches(arg, &tool.call.arguments)),
        Condition::ResultContains(needle) => tool
            .and_then(|tool| tool.result)
            .is_some_and(|result| result.contains(needle.as_str())),
        Condition::Turn(comparison) => comparison.holds(run.turns),
        Condition::Calls(count) => {
            let calls = run
                .executed
                .iter()
                .filter(|call| {
                    count.outcome.admits(call.failed) && tool_name_matches(&count.tool, &call.tool)
                })
                .count();
            count.comparison().holds(saturate(calls))
        }
        Condition::TurnsSinceCall(count) => {
            let last = run
                .executed
                .iter()
                .rev()
                .find(|call| {
                    count.outcome.admits(call.failed) && tool_name_matches(&count.tool, &call.tool)
                })
                .map_or(0, |call| call.turn);
            count.comparison().holds(run.turns.saturating_sub(last))
        }
        Condition::SinceLastCall(since) => {
            let Some(position) = run
                .executed
                .iter()
                .rposition(|call| tool_name_matches(&since.of, &call.tool))
            else {
                return false;
            };
            let calls = run.executed[position + 1..]
                .iter()
                .filter(|call| {
                    since.outcome.admits(call.failed)
                        && tool_name_matches(&since.called, &call.tool)
                })
                .count();
            since.comparison().holds(saturate(calls))
        }
        Condition::ToolOffered(names) => run
            .offered_tools
            .iter()
            .any(|tool| tool_name_matches(names, tool)),
        Condition::RepeatedCall(comparison) => {
            comparison.holds(run.streak.as_ref().map_or(0, |streak| streak.count))
        }
        Condition::ProfileEnteredFrom(names) => run
            .entered_from
            .as_deref()
            .is_some_and(|from| name_matches(names, from)),
        Condition::FinalStatus(names) => run
            .final_status
            .as_deref()
            .is_some_and(|status| !status.is_empty() && name_matches(names, status)),
        Condition::All(conditions) => conditions.iter().all(|c| holds(c, tool, run)),
        Condition::Any(conditions) => conditions.iter().any(|c| holds(c, tool, run)),
        Condition::Not(condition) => !holds(condition, tool, run),
    }
}

fn saturate(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

pub(crate) fn name_matches(names: &NameMatch, name: &str) -> bool {
    names
        .patterns()
        .iter()
        .any(|pattern| glob_match(pattern, name))
}

/// [`name_matches`] for a *tool* name: a pattern that matches the tool an
/// alias follows (`write_file`) also matches the alias (`edit_file`), so a
/// rule written for one cannot be sidestepped by calling the other.
pub(crate) fn tool_name_matches(names: &NameMatch, tool: &str) -> bool {
    name_matches(names, tool)
        || followed_tool(tool).is_some_and(|followed| name_matches(names, followed))
}

fn arg_matches(arg: &ArgMatch, arguments: &Value) -> bool {
    let Some(value) = arguments.pointer(&arg.pointer) else {
        return false;
    };
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    arg.glob
        .as_deref()
        .is_none_or(|pattern| glob_match(pattern, &text))
        && arg
            .contains
            .as_deref()
            .is_none_or(|needle| text.contains(needle))
        && arg.equals.as_ref().is_none_or(|expected| value == expected)
}

/// `*` matches within a `/`-separated segment, `**` across segments, `?`
/// one non-`/` character. Everything else is literal.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    fn matches(pattern: &[char], text: &[char]) -> bool {
        match pattern {
            [] => text.is_empty(),
            ['*', '*', rest @ ..] => {
                let rest = rest.strip_prefix(&['/']).unwrap_or(rest);
                (0..=text.len()).any(|skip| matches(rest, &text[skip..]))
                    || (pattern.len() > 2 && matches(&pattern[2..], text))
            }
            ['*', rest @ ..] => (0..=text.len())
                .take_while(|&skip| skip == 0 || text[skip - 1] != '/')
                .any(|skip| matches(rest, &text[skip..])),
            ['?', rest @ ..] => {
                matches!(text, [first, ..] if *first != '/') && matches(rest, &text[1..])
            }
            [literal, rest @ ..] => {
                matches!(text, [first, ..] if first == literal) && matches(rest, &text[1..])
            }
        }
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    matches(&pattern, &text)
}
