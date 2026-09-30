//! Pure rule evaluation. Given an event and the run's state, decide which
//! rules fire and what they ask for. No I/O; the caller applies the outcome.

use harness_protocol::tools::ToolCall;
use serde_json::Value;

use super::definition::{Action, ArgMatch, Condition, NameMatch, Placement, RuleEvent};
use super::state::{BehaviorState, RunCounters};

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
    let target = target.as_object_mut().expect("made an object above");
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

impl BehaviorState {
    /// Evaluate the active profile's rules for `event`, recording firings
    /// (for `max_fires`) in the run state.
    pub fn evaluate(&mut self, event: RuleEvent, tool: Option<ToolEvent<'_>>) -> RuleOutcome {
        let profile = self.profile.clone();
        let reference = profile.reference();
        let mut outcome = RuleOutcome::default();
        for rule in profile.profile.rules.iter().filter(|rule| rule.on == event) {
            let fired_so_far = self.run.fired.get(&rule.id).copied().unwrap_or(0);
            if rule.max_fires.is_some_and(|max| fired_so_far >= max) {
                continue;
            }
            if let Some(condition) = &rule.when {
                if !holds(condition, tool, &self.run) {
                    continue;
                }
            }
            let is_decision = matches!(
                rule.action,
                Action::Deny { .. } | Action::Ask { .. } | Action::Allow {}
            );
            // The first decision and the first switch win; later ones don't fire.
            if (is_decision && outcome.decision.is_some())
                || (matches!(rule.action, Action::SwitchProfile { .. }) && outcome.switch.is_some())
            {
                continue;
            }
            *self.run.fired.entry(rule.id.clone()).or_default() += 1;
            outcome.fired.push(FiredRule {
                rule_id: rule.id.clone(),
                event,
                action: rule.action.name(),
            });
            let rule_id = rule.id.clone();
            match &rule.action {
                Action::Inject(inject) => outcome.injections.push(Injection {
                    placement: inject.placement.unwrap_or(Placement::default_for(event)),
                    text: system_reminder(
                        &format!("profile:{reference} rule:{}", rule.id),
                        &inject.text,
                    ),
                    rule_id,
                }),
                Action::Deny { reason } => {
                    outcome.decision = Some(ToolDecision::Deny {
                        rule_id,
                        reason: reason.clone(),
                    })
                }
                Action::Ask { .. } => outcome.decision = Some(ToolDecision::Ask { rule_id }),
                Action::Allow {} => outcome.decision = Some(ToolDecision::Allow { rule_id }),
                Action::StopRun { reason } => {
                    outcome.stop = Some(RuleStop {
                        rule_id,
                        reason: reason.clone(),
                    });
                    break;
                }
                Action::SwitchProfile { profile } => {
                    outcome.switch = Some((rule_id, profile.clone()))
                }
                Action::RewriteArgs { merge } => {
                    outcome.rewrite_args.push((rule_id, merge.clone()))
                }
                Action::RewriteResult { replace, append } => {
                    let rewrite = match (replace, append) {
                        (Some(text), _) => ResultRewrite::Replace(text.clone()),
                        (None, Some(text)) => ResultRewrite::Append(text.clone()),
                        (None, None) => continue,
                    };
                    outcome.rewrite_result.push((rule_id, rewrite));
                }
            }
        }
        outcome
    }

    /// Evaluate `ProfileEntered` rules; `from` is the previous profile's id.
    pub fn evaluate_entered(&mut self, from: &str) -> RuleOutcome {
        self.run.entered_from = Some(from.to_string());
        let outcome = self.evaluate(RuleEvent::ProfileEntered, None);
        self.run.entered_from = None;
        outcome
    }

    /// Whether `condition` holds for the run as it stands (no tool event).
    /// Used by completion-gate `require` checks.
    pub fn condition_holds(&self, condition: &Condition) -> bool {
        holds(condition, None, &self.run)
    }

    /// Record a tool request for loop detection. Call before evaluating
    /// `PreToolUse`, so `repeated_call` counts the current request.
    pub fn note_tool_request(&mut self, call: &ToolCall) {
        // serde_json maps are ordered, so this is canonical for equal args.
        let signature = format!("{}\u{0}{}", call.name, call.arguments);
        match &mut self.run.streak {
            Some(streak) if streak.signature == signature => streak.count += 1,
            _ => {
                self.run.streak = Some(super::state::CallStreak {
                    signature,
                    count: 1,
                })
            }
        }
    }

    /// Record that a tool actually executed (not denied) in this turn.
    pub fn note_executed(&mut self, tool: &str) {
        let turn = self.run.turns;
        self.run.executed.push(super::state::ExecutedCall {
            turn,
            tool: tool.to_string(),
        });
    }
}

fn holds(condition: &Condition, tool: Option<ToolEvent<'_>>, run: &RunCounters) -> bool {
    match condition {
        Condition::Tool(names) => tool.is_some_and(|tool| name_matches(names, &tool.call.name)),
        Condition::Arg(arg) => tool.is_some_and(|tool| arg_matches(arg, &tool.call.arguments)),
        Condition::ResultContains(needle) => tool
            .and_then(|tool| tool.result)
            .is_some_and(|result| result.contains(needle.as_str())),
        Condition::Turn(comparison) => comparison.holds(run.turns),
        Condition::Calls(count) => {
            let calls = run
                .executed
                .iter()
                .filter(|call| name_matches(&count.tool, &call.tool))
                .count();
            count.comparison().holds(saturate(calls))
        }
        Condition::TurnsSinceCall(count) => {
            let last = run
                .executed
                .iter()
                .rev()
                .find(|call| name_matches(&count.tool, &call.tool))
                .map_or(0, |call| call.turn);
            count.comparison().holds(run.turns.saturating_sub(last))
        }
        Condition::SinceLastCall(since) => {
            let Some(position) = run
                .executed
                .iter()
                .rposition(|call| name_matches(&since.of, &call.tool))
            else {
                return false;
            };
            let calls = run.executed[position + 1..]
                .iter()
                .filter(|call| name_matches(&since.called, &call.tool))
                .count();
            since.comparison().holds(saturate(calls))
        }
        Condition::RepeatedCall(comparison) => {
            comparison.holds(run.streak.as_ref().map_or(0, |streak| streak.count))
        }
        Condition::ProfileEnteredFrom(names) => run
            .entered_from
            .as_deref()
            .is_some_and(|from| name_matches(names, from)),
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
        .map_or(true, |pattern| glob_match(pattern, &text))
        && arg
            .contains
            .as_deref()
            .map_or(true, |needle| text.contains(needle))
        && arg
            .equals
            .as_ref()
            .map_or(true, |expected| value == expected)
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
