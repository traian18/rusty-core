//! Checks on a profile's rules, their actions, and the child policy.

use std::collections::BTreeSet;

use super::{condition_checks::check_condition, gate_checks::check_gate, Issues};
use crate::behavior::definition::{Action, BehaviorProfile, ChildPolicy, Inject, RuleEvent};

pub(super) fn check_rules_and_children(profile: &BehaviorProfile, issues: &mut Issues) {
    check_rules(profile, issues);
    if let Some(gate) = &profile.completion_gate {
        check_gate(gate, issues);
    }
    if !matches!(profile.children, ChildPolicy::Inherit) {
        issues.add(
            "children.type",
            "unsupported_child_policy",
            format!(
                "child policy `{}` is reserved; only `inherit` is supported",
                profile.children.type_name()
            ),
        );
    }
}

fn check_rules(profile: &BehaviorProfile, issues: &mut Issues) {
    let mut rule_ids = BTreeSet::new();
    for (index, rule) in profile.rules.iter().enumerate() {
        let path = format!("rules[{index}]");
        if rule.id.trim().is_empty() {
            issues.add(&format!("{path}.id"), "empty_id", "rule id cannot be empty");
        } else if !rule_ids.insert(rule.id.as_str()) {
            issues.add(
                &format!("{path}.id"),
                "duplicate_rule_id",
                format!("rule id {} is used twice", rule.id),
            );
        }
        if rule.max_fires == Some(0) {
            issues.add(
                &format!("{path}.max_fires"),
                "invalid_limit",
                "max_fires must be at least 1",
            );
        }
        if let Some(condition) = &rule.when {
            check_condition(condition, rule.on, &format!("{path}.when"), issues);
        }
        check_action(&rule.action, rule.on, &format!("{path}.do"), issues);
    }
}

fn check_action(action: &Action, event: RuleEvent, path: &str, issues: &mut Issues) {
    match action {
        Action::Inject(inject) => check_inject(inject, event, path, issues),
        Action::Deny { reason } => {
            issues.require_text(path, reason, "deny reason cannot be empty");
            require_pre_tool_use(event, "deny", path, issues);
        }
        Action::Ask { .. } | Action::Allow {} => {
            require_pre_tool_use(event, action.name(), path, issues);
        }
        Action::StopRun { reason } => {
            issues.require_text(path, reason, "stop_run reason cannot be empty");
        }
        Action::SwitchProfile { profile } => {
            if profile.id.as_str().trim().is_empty() {
                issues.add(
                    path,
                    "invalid_profile_reference",
                    "switch target id cannot be empty",
                );
            }
        }
        Action::RewriteArgs { merge } => {
            require_pre_tool_use(event, "rewrite_args", path, issues);
            if !merge.is_object() {
                issues.add(
                    path,
                    "invalid_merge_patch",
                    "rewrite_args.merge must be a JSON object (RFC 7396 merge patch)",
                );
            }
        }
        Action::RewriteResult { replace, append } => {
            let after_tool = matches!(
                event,
                RuleEvent::PostToolUse | RuleEvent::PostToolUseFailure
            );
            if !after_tool {
                issues.add(
                    path,
                    "action_not_available",
                    "rewrite_result is only available on post-tool events",
                );
            }
            if replace.is_some() == append.is_some() {
                issues.add(
                    path,
                    "invalid_rewrite",
                    "rewrite_result needs exactly one of replace or append",
                );
            }
        }
    }
}

fn check_inject(inject: &Inject, event: RuleEvent, path: &str, issues: &mut Issues) {
    issues.require_text(path, &inject.text, "inject text cannot be empty");
    if let Some(placement) = inject.placement {
        if !placement.allowed_on(event) {
            issues.add(
                path,
                "placement_not_available",
                format!(
                    "placement {} is not available on {}",
                    placement.name(),
                    event.name()
                ),
            );
        }
    }
}

fn require_pre_tool_use(event: RuleEvent, action: &str, path: &str, issues: &mut Issues) {
    if event != RuleEvent::PreToolUse {
        issues.add(
            path,
            "action_not_available",
            format!("{action} is only available on PreToolUse"),
        );
    }
}
