//! Pure rule evaluation. Given an event and the run's state, decide which rules fire and what
//! they ask for. No I/O; the caller applies the outcome.
//!
//! The outcome types are here. `evaluate` decides which rules fire, `conditions` answers whether
//! a condition holds, `notes` records what the run did so conditions can see it, and
//! `final_status` reads the status line of a final answer.

pub(super) mod conditions;
mod evaluate;
mod final_status;
mod notes;

use rusty_protocol::tools::ToolCall;
use serde_json::Value;

use super::definition::{Placement, RuleEvent};

pub use final_status::final_status;

/// The tool involved in a tool event.
#[derive(Debug, Clone, Copy)]
pub struct ToolEvent<'a> {
    pub call: &'a ToolCall,
    /// Result text, for post-tool events.
    pub result: Option<&'a str>,
}

/// What the rules for one event asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleOutcome {
    /// The first `deny` / `ask` / `allow` that fired (`PreToolUse` only).
    pub decision: Option<ToolDecision>,
    /// The first `stop_run` that fired. Evaluation stops there.
    pub stop: Option<RuleStop>,
    /// Context to add, already wrapped in a `<system-reminder>`.
    pub injections: Vec<Injection>,
    /// Every rule that fired, in order, for observability.
    pub fired: Vec<FiredRule>,
    /// The first `switch_profile` that fired: `(rule_id, target)`.
    pub switch: Option<(String, super::definition::ProfileRef)>,
    /// Every `rewrite_args` merge patch that fired, in order.
    pub rewrite_args: Vec<(String, Value)>,
    /// Every `rewrite_result` that fired, in order.
    pub rewrite_result: Vec<(String, ResultRewrite)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultRewrite {
    Replace(String),
    Append(String),
}

impl ResultRewrite {
    /// Apply to a tool result's text.
    pub fn apply(&self, text: &mut String) {
        match self {
            Self::Replace(replacement) => *text = replacement.clone(),
            Self::Append(extra) => {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(extra);
            }
        }
    }
}

/// RFC 7396 JSON merge patch: objects merge recursively, `null` deletes a
/// key, anything else replaces.
pub fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Default::default());
    }
    let Some(target) = target.as_object_mut() else {
        return;
    };
    for (key, value) in patch {
        if value.is_null() {
            target.remove(key);
        } else {
            merge_patch(target.entry(key.clone()).or_insert(Value::Null), value);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolDecision {
    Deny { rule_id: String, reason: String },
    Ask { rule_id: String },
    Allow { rule_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleStop {
    pub rule_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Injection {
    pub rule_id: String,
    pub placement: Placement,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FiredRule {
    pub rule_id: String,
    pub event: RuleEvent,
    pub action: &'static str,
}

/// Wrap harness-authored text so the model, UIs, and replay can tell it
/// apart from user input.
pub fn system_reminder(source: &str, text: &str) -> String {
    format!("<system-reminder source=\"{source}\">{text}</system-reminder>")
}
