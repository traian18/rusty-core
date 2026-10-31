//! Checks on the parts of a profile that are not rules: identity, instructions, tools, limits.

use std::collections::BTreeSet;

use super::Issues;
use crate::behavior::{
    definition::{BehaviorProfile, InstructionMode, BEHAVIOR_SCHEMA_VERSION},
    ToolScope,
};

pub(super) fn check_identity(profile: &BehaviorProfile, issues: &mut Issues) {
    if profile.schema_version != BEHAVIOR_SCHEMA_VERSION {
        issues.add(
            "schema_version",
            "unsupported_schema_version",
            format!(
                "expected version {BEHAVIOR_SCHEMA_VERSION}, got {}",
                profile.schema_version
            ),
        );
    }
    if profile.id.as_str().trim().is_empty() {
        issues.add("id", "empty_id", "profile id cannot be empty");
    }
    if profile.revision == 0 {
        issues.add(
            "revision",
            "invalid_revision",
            "revision must be at least 1",
        );
    }
    if profile.name.trim().is_empty() {
        issues.add("name", "empty_name", "profile name cannot be empty");
    }
}

pub(super) fn check_instructions(profile: &BehaviorProfile, issues: &mut Issues) {
    for (family, text) in &profile.instructions.variants {
        let path = format!("instructions.variants.{family}");
        if family.trim().is_empty() || *family != family.to_lowercase() {
            issues.add(
                &path,
                "invalid_variant_key",
                "variant keys must be non-empty lowercase model-family substrings",
            );
        }
        if text.trim().is_empty() {
            issues.add(&path, "empty_variant", "variant text cannot be empty");
        }
    }
    let instructions = &profile.instructions;
    let would_erase_the_prompt = instructions.mode == InstructionMode::Replace
        && instructions.text.trim().is_empty()
        && instructions.variants.is_empty();
    if would_erase_the_prompt {
        issues.add(
            "instructions.text",
            "empty_replacement",
            "replace mode needs instruction text; it would otherwise erase the system prompt",
        );
    }
}

pub(super) fn check_tools(profile: &BehaviorProfile, issues: &mut Issues) {
    if let ToolScope::AllowList(tools) = &profile.tools {
        check_allow_list(tools, issues);
    }
    for (tool, tool_override) in &profile.tool_overrides {
        let path = format!("tool_overrides.{tool}");
        if tool.trim().is_empty() {
            issues.add(&path, "invalid_tool_reference", "tool ids cannot be empty");
        }
        let blank_append = tool_override
            .description_append
            .as_deref()
            .is_some_and(|text| text.trim().is_empty());
        if blank_append {
            issues.add(
                &path,
                "empty_description_append",
                "description_append cannot be empty",
            );
        }
    }
}

fn check_allow_list(tools: &[String], issues: &mut Issues) {
    let mut seen = BTreeSet::new();
    for tool in tools {
        if tool.trim().is_empty() {
            issues.add(
                "tools",
                "invalid_tool_reference",
                "tool ids cannot be empty",
            );
        } else if !seen.insert(tool) {
            issues.add(
                "tools",
                "duplicate_tool",
                format!("tool {tool} is listed twice"),
            );
        }
    }
}

pub(super) fn check_execution_and_limits(profile: &BehaviorProfile, issues: &mut Issues) {
    let execution = &profile.execution;
    if let Some(temperature) = execution.temperature {
        if !(0.0..=2.0).contains(&temperature) {
            issues.add(
                "execution.temperature",
                "invalid_temperature",
                "temperature must be between 0 and 2",
            );
        }
    }
    if execution.max_tokens == Some(0) {
        issues.add(
            "execution.max_tokens",
            "invalid_max_tokens",
            "max_tokens must be at least 1",
        );
    }
    if is_blank(execution.model.as_deref()) {
        issues.add("execution.model", "empty_model", "model cannot be empty");
    }
    let limits = &profile.limits;
    if limits.max_turns == Some(0) {
        issues.add(
            "limits.max_turns",
            "invalid_limit",
            "max_turns must be at least 1",
        );
    }
    if is_blank(limits.final_turn_prompt.as_deref()) {
        issues.add(
            "limits.final_turn_prompt",
            "empty_final_turn_prompt",
            "final_turn_prompt cannot be empty",
        );
    }
}

/// Present but empty: an unset option is fine, a blank one is a mistake.
fn is_blank(text: Option<&str>) -> bool {
    text.is_some_and(|text| text.trim().is_empty())
}
