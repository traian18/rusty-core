//! Deciding which rules fire for an event, and what they ask for.

use super::{
    conditions::holds, system_reminder, FiredRule, Injection, ResultRewrite, RuleOutcome, RuleStop,
    ToolDecision, ToolEvent,
};
use crate::behavior::{
    definition::{Action, Condition, Placement, ProfileRef, Rule, RuleEvent},
    state::BehaviorState,
};

impl BehaviorState {
    /// Evaluates the active profile's rules for `event` and records each firing in the run state,
    /// which is how `max_fires` is enforced.
    pub fn evaluate(&mut self, event: RuleEvent, tool: Option<ToolEvent<'_>>) -> RuleOutcome {
        let profile = self.profile.clone();
        let reference = profile.reference();
        let mut outcome = RuleOutcome::default();
        for rule in profile.profile.rules.iter().filter(|rule| rule.on == event) {
            if !self.rule_may_fire(rule, tool, &outcome) {
                continue;
            }
            *self.run.fired.entry(rule.id.clone()).or_default() += 1;
            outcome.fired.push(FiredRule {
                rule_id: rule.id.clone(),
                event,
                action: rule.action.name(),
            });
            if apply_action(&mut outcome, rule, event, &reference) == Next::Stop {
                break;
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

    /// A rule fires unless it is used up, its condition fails, or an earlier rule already took the
    /// one decision or switch an event allows.
    fn rule_may_fire(
        &self,
        rule: &Rule,
        tool: Option<ToolEvent<'_>>,
        outcome: &RuleOutcome,
    ) -> bool {
        let fired_so_far = self.run.fired.get(&rule.id).copied().unwrap_or(0);
        if rule.max_fires.is_some_and(|max| fired_so_far >= max) {
            return false;
        }
        if let Some(condition) = &rule.when {
            if !holds(condition, tool, &self.run) {
                return false;
            }
        }
        !already_decided(rule, outcome)
    }
}

fn already_decided(rule: &Rule, outcome: &RuleOutcome) -> bool {
    let is_decision = matches!(
        rule.action,
        Action::Deny { .. } | Action::Ask { .. } | Action::Allow {}
    );
    let is_switch = matches!(rule.action, Action::SwitchProfile { .. });
    (is_decision && outcome.decision.is_some()) || (is_switch && outcome.switch.is_some())
}

/// Whether the remaining rules still get a turn.
#[derive(PartialEq, Eq)]
enum Next {
    Continue,
    Stop,
}

/// Records what a firing rule asks for. Stopping the run ends the evaluation.
fn apply_action(
    outcome: &mut RuleOutcome,
    rule: &Rule,
    event: RuleEvent,
    reference: &ProfileRef,
) -> Next {
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
            });
        }
        Action::Ask { .. } => outcome.decision = Some(ToolDecision::Ask { rule_id }),
        Action::Allow {} => outcome.decision = Some(ToolDecision::Allow { rule_id }),
        Action::StopRun { reason } => {
            outcome.stop = Some(RuleStop {
                rule_id,
                reason: reason.clone(),
            });
            return Next::Stop;
        }
        Action::SwitchProfile { profile } => outcome.switch = Some((rule_id, profile.clone())),
        Action::RewriteArgs { merge } => outcome.rewrite_args.push((rule_id, merge.clone())),
        Action::RewriteResult { replace, append } => match (replace, append) {
            (Some(text), _) => outcome
                .rewrite_result
                .push((rule_id, ResultRewrite::Replace(text.clone()))),
            (None, Some(text)) => outcome
                .rewrite_result
                .push((rule_id, ResultRewrite::Append(text.clone()))),
            (None, None) => {}
        },
    }
    Next::Continue
}
