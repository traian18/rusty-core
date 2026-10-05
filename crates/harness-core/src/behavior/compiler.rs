use std::collections::BTreeSet;

use harness_protocol::backend::{ExecutionParams, ReasoningEffort};
use harness_protocol::tools::PermissionMode;

use crate::orchestration::ToolScope;
use crate::tool_alias::granted_by;

use super::definition::{
    Action, BehaviorProfile, ChildPolicy, Comparison, CompletionGate, Condition, EvaluatorSpec,
    InstructionMode, NameMatch, ProfileRef, ReasoningEffortSetting, RuleEvent, ToolPermission,
    BEHAVIOR_SCHEMA_VERSION,
};

/// A validated profile plus its content hash. Immutable; shared by `Arc`.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledProfile {
    pub profile: BehaviorProfile,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileIssue {
    pub path: String,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileValidationError {
    pub issues: Vec<ProfileIssue>,
}

impl std::fmt::Display for ProfileValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid behavior profile")?;
        for issue in &self.issues {
            write!(formatter, "; {}: {}", issue.path, issue.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for ProfileValidationError {}

/// Validate a profile, reporting every issue found rather than the first.
pub fn compile(profile: BehaviorProfile) -> Result<CompiledProfile, ProfileValidationError> {
    let mut issues = Vec::new();
    let mut issue = |path: &str, code: &'static str, message: String| {
        issues.push(ProfileIssue {
            path: path.to_string(),
            code,
            message,
        })
    };

    if profile.schema_version != BEHAVIOR_SCHEMA_VERSION {
        issue(
            "schema_version",
            "unsupported_schema_version",
            format!(
                "expected version {BEHAVIOR_SCHEMA_VERSION}, got {}",
                profile.schema_version
            ),
        );
    }
    if profile.id.as_str().trim().is_empty() {
        issue("id", "empty_id", "profile id cannot be empty".into());
    }
    if profile.revision == 0 {
        issue(
            "revision",
            "invalid_revision",
            "revision must be at least 1".into(),
        );
    }
    if profile.name.trim().is_empty() {
        issue("name", "empty_name", "profile name cannot be empty".into());
    }

    for (family, text) in &profile.instructions.variants {
        if family.trim().is_empty() || *family != family.to_lowercase() {
            issue(
                &format!("instructions.variants.{family}"),
                "invalid_variant_key",
                "variant keys must be non-empty lowercase model-family substrings".into(),
            );
        }
        if text.trim().is_empty() {
            issue(
                &format!("instructions.variants.{family}"),
                "empty_variant",
                "variant text cannot be empty".into(),
            );
        }
    }
    if profile.instructions.mode == InstructionMode::Replace
        && profile.instructions.text.trim().is_empty()
        && profile.instructions.variants.is_empty()
    {
        issue(
            "instructions.text",
            "empty_replacement",
            "replace mode needs instruction text; it would otherwise erase the system prompt"
                .into(),
        );
    }

    if let ToolScope::AllowList(tools) = &profile.tools {
        let mut seen = BTreeSet::new();
        for tool in tools {
            if tool.trim().is_empty() {
                issue(
                    "tools",
                    "invalid_tool_reference",
                    "tool ids cannot be empty".into(),
                );
            } else if !seen.insert(tool) {
                issue(
                    "tools",
                    "duplicate_tool",
                    format!("tool {tool} is listed twice"),
                );
            }
        }
    }
    for (tool, override_) in &profile.tool_overrides {
        let path = format!("tool_overrides.{tool}");
        if tool.trim().is_empty() {
            issue(
                &path,
                "invalid_tool_reference",
                "tool ids cannot be empty".into(),
            );
        }
        if override_
            .description_append
            .as_deref()
            .is_some_and(|text| text.trim().is_empty())
        {
            issue(
                &path,
                "empty_description_append",
                "description_append cannot be empty".into(),
            );
        }
    }

    if let Some(temperature) = profile.execution.temperature {
        if !(0.0..=2.0).contains(&temperature) {
            issue(
                "execution.temperature",
                "invalid_temperature",
                "temperature must be between 0 and 2".into(),
            );
        }
    }
    if profile.execution.max_tokens == Some(0) {
        issue(
            "execution.max_tokens",
            "invalid_max_tokens",
            "max_tokens must be at least 1".into(),
        );
    }
    if profile
        .execution
        .model
        .as_deref()
        .is_some_and(|model| model.trim().is_empty())
    {
        issue(
            "execution.model",
            "empty_model",
            "model cannot be empty".into(),
        );
    }

    if profile.limits.max_turns == Some(0) {
        issue(
            "limits.max_turns",
            "invalid_limit",
            "max_turns must be at least 1".into(),
        );
    }
    if profile
        .limits
        .final_turn_prompt
        .as_deref()
        .is_some_and(|text| text.trim().is_empty())
    {
        issue(
            "limits.final_turn_prompt",
            "empty_final_turn_prompt",
            "final_turn_prompt cannot be empty".into(),
        );
    }

    let mut rule_ids = BTreeSet::new();
    for (index, rule) in profile.rules.iter().enumerate() {
        let path = format!("rules[{index}]");
        if rule.id.trim().is_empty() {
            issue(
                &format!("{path}.id"),
                "empty_id",
                "rule id cannot be empty".into(),
            );
        } else if !rule_ids.insert(rule.id.as_str()) {
            issue(
                &format!("{path}.id"),
                "duplicate_rule_id",
                format!("rule id {} is used twice", rule.id),
            );
        }
        if rule.max_fires == Some(0) {
            issue(
                &format!("{path}.max_fires"),
                "invalid_limit",
                "max_fires must be at least 1".into(),
            );
        }
        if let Some(condition) = &rule.when {
            validate_condition(condition, rule.on, &format!("{path}.when"), &mut issue);
        }
        validate_action(&rule.action, rule.on, &format!("{path}.do"), &mut issue);
    }
    if let Some(gate) = &profile.completion_gate {
        validate_gate(gate, &mut issue);
    }
    if !matches!(profile.children, ChildPolicy::Inherit) {
        issue(
            "children.type",
            "unsupported_child_policy",
            format!(
                "child policy `{}` is reserved; only `inherit` is supported",
                profile.children.type_name()
            ),
        );
    }

    if issues.is_empty() {
        let content_hash = crate::content_hash::content_hash(&profile);
        Ok(CompiledProfile {
            profile,
            content_hash,
        })
    } else {
        Err(ProfileValidationError { issues })
    }
}

fn validate_names(
    names: &NameMatch,
    path: &str,
    issue: &mut impl FnMut(&str, &'static str, String),
) {
    if names.patterns().is_empty() || names.patterns().iter().any(|p| p.trim().is_empty()) {
        issue(
            path,
            "invalid_tool_reference",
            "tool patterns cannot be empty".into(),
        );
    }
}

fn validate_comparison(
    comparison: &Comparison,
    path: &str,
    issue: &mut impl FnMut(&str, &'static str, String),
) {
    if comparison.is_empty() {
        issue(
            path,
            "empty_comparison",
            "give at least one of eq, gte, lte".into(),
        );
    }
}

fn validate_condition(
    condition: &Condition,
    event: RuleEvent,
    path: &str,
    issue: &mut impl FnMut(&str, &'static str, String),
) {
    let requires_tool_event = |issue: &mut dyn FnMut(&str, &'static str, String), what: &str| {
        if !event.is_tool_event() {
            issue(
                path,
                "condition_not_available",
                format!("{what} needs a tool event, not {}", event.name()),
            );
        }
    };
    match condition {
        Condition::Tool(names) => {
            requires_tool_event(issue, "tool");
            validate_names(names, &format!("{path}.tool"), issue);
        }
        Condition::Arg(arg) => {
            requires_tool_event(issue, "arg");
            if !arg.pointer.is_empty() && !arg.pointer.starts_with('/') {
                issue(
                    &format!("{path}.arg.pointer"),
                    "invalid_pointer",
                    "use JSON Pointer syntax: empty or starting with '/'".into(),
                );
            }
            if arg.glob.is_none() && arg.contains.is_none() && arg.equals.is_none() {
                issue(
                    &format!("{path}.arg"),
                    "empty_match",
                    "give at least one of glob, contains, equals".into(),
                );
            }
        }
        Condition::ResultContains(_) => {
            if !matches!(
                event,
                RuleEvent::PostToolUse | RuleEvent::PostToolUseFailure
            ) {
                issue(
                    path,
                    "condition_not_available",
                    format!(
                        "result_contains needs a post-tool event, not {}",
                        event.name()
                    ),
                );
            }
        }
        Condition::RepeatedCall(comparison) => {
            if event != RuleEvent::PreToolUse {
                issue(
                    path,
                    "condition_not_available",
                    format!("repeated_call needs PreToolUse, not {}", event.name()),
                );
            }
            validate_comparison(comparison, &format!("{path}.repeated_call"), issue);
        }
        Condition::Turn(comparison) => {
            validate_comparison(comparison, &format!("{path}.turn"), issue)
        }
        Condition::ToolOffered(names) => {
            validate_names(names, &format!("{path}.tool_offered"), issue)
        }
        Condition::Calls(count) | Condition::TurnsSinceCall(count) => {
            validate_names(&count.tool, &format!("{path}.tool"), issue);
            validate_comparison(&count.comparison(), path, issue);
        }
        Condition::SinceLastCall(since) => {
            validate_names(&since.of, &format!("{path}.since_last_call.of"), issue);
            validate_names(
                &since.called,
                &format!("{path}.since_last_call.called"),
                issue,
            );
            validate_comparison(
                &since.comparison(),
                &format!("{path}.since_last_call"),
                issue,
            );
        }
        Condition::All(conditions) | Condition::Any(conditions) => {
            if conditions.is_empty() {
                issue(
                    path,
                    "empty_combinator",
                    "all/any need at least one condition".into(),
                );
            }
            for (index, condition) in conditions.iter().enumerate() {
                validate_condition(condition, event, &format!("{path}[{index}]"), issue);
            }
        }
        Condition::Not(condition) => {
            validate_condition(condition, event, &format!("{path}.not"), issue)
        }
        Condition::ProfileEnteredFrom(names) => {
            if event != RuleEvent::ProfileEntered {
                issue(
                    path,
                    "condition_not_available",
                    format!(
                        "profile_entered_from needs ProfileEntered, not {}",
                        event.name()
                    ),
                );
            }
            validate_names(names, &format!("{path}.profile_entered_from"), issue);
        }
    }
}

fn validate_gate(gate: &CompletionGate, issue: &mut impl FnMut(&str, &'static str, String)) {
    if gate.checks.is_empty() {
        issue(
            "completion_gate.checks",
            "empty_gate",
            "a completion gate needs at least one check".into(),
        );
    }
    let mut ids = BTreeSet::new();
    for (index, check) in gate.checks.iter().enumerate() {
        let path = format!("completion_gate.checks[{index}]");
        if check.id.trim().is_empty() {
            issue(
                &format!("{path}.id"),
                "empty_id",
                "check id cannot be empty".into(),
            );
        } else if !ids.insert(check.id.as_str()) {
            issue(
                &format!("{path}.id"),
                "duplicate_check_id",
                format!("check id {} is used twice", check.id),
            );
        }
        match (&check.require, &check.evaluator) {
            (Some(condition), None) => {
                validate_gate_condition(condition, &format!("{path}.require"), issue);
                if check
                    .feedback
                    .as_deref()
                    .map_or(true, |text| text.trim().is_empty())
                {
                    issue(
                        &format!("{path}.feedback"),
                        "missing_feedback",
                        "a `require` check needs feedback to send back when it fails".into(),
                    );
                }
            }
            (None, Some(evaluator)) => {
                validate_evaluator(evaluator, &format!("{path}.evaluator"), issue)
            }
            _ => issue(
                &path,
                "invalid_check",
                "a check needs exactly one of `require` or `evaluator`".into(),
            ),
        }
    }
}

/// Gate conditions are evaluated when the model proposes to finish, so
/// there is no tool call to inspect.
fn validate_gate_condition(
    condition: &Condition,
    path: &str,
    issue: &mut impl FnMut(&str, &'static str, String),
) {
    match condition {
        Condition::Tool(_)
        | Condition::Arg(_)
        | Condition::ResultContains(_)
        | Condition::RepeatedCall(_)
        | Condition::ProfileEnteredFrom(_) => issue(
            path,
            "condition_not_available",
            "tool, arg, result_contains and repeated_call are not available in a completion gate"
                .into(),
        ),
        Condition::All(conditions) | Condition::Any(conditions) => {
            for (index, condition) in conditions.iter().enumerate() {
                validate_gate_condition(condition, &format!("{path}[{index}]"), issue);
            }
        }
        Condition::Not(condition) => {
            validate_gate_condition(condition, &format!("{path}.not"), issue)
        }
        Condition::Turn(_)
        | Condition::Calls(_)
        | Condition::TurnsSinceCall(_)
        | Condition::SinceLastCall(_)
        | Condition::ToolOffered(_) => {}
    }
    // The shared checks (comparisons, name patterns, empty combinators).
    validate_condition(condition, RuleEvent::BeforeModelRequest, path, issue);
}

fn validate_evaluator(
    evaluator: &EvaluatorSpec,
    path: &str,
    issue: &mut impl FnMut(&str, &'static str, String),
) {
    match evaluator {
        EvaluatorSpec::Tool { tool, .. } => {
            if tool.trim().is_empty() {
                issue(
                    path,
                    "invalid_tool_reference",
                    "evaluator tool cannot be empty".into(),
                );
            }
        }
        EvaluatorSpec::Model {
            instructions,
            model,
            transcript_messages,
        } => {
            if instructions.trim().is_empty() {
                issue(
                    path,
                    "empty_text",
                    "model evaluator instructions cannot be empty".into(),
                );
            }
            if model
                .as_deref()
                .is_some_and(|model| model.trim().is_empty())
            {
                issue(path, "empty_model", "model cannot be empty".into());
            }
            if *transcript_messages > 200 {
                issue(
                    path,
                    "invalid_limit",
                    "transcript_messages must be at most 200".into(),
                );
            }
        }
        EvaluatorSpec::Agent {
            instructions,
            tools,
            model,
            max_turns,
            transcript_messages,
        } => {
            if instructions.trim().is_empty() {
                issue(
                    path,
                    "empty_text",
                    "agent evaluator instructions cannot be empty".into(),
                );
            }
            if tools.iter().any(|tool| tool.trim().is_empty()) {
                issue(
                    path,
                    "invalid_tool_reference",
                    "tool ids cannot be empty".into(),
                );
            }
            if model
                .as_deref()
                .is_some_and(|model| model.trim().is_empty())
            {
                issue(path, "empty_model", "model cannot be empty".into());
            }
            if !(1..=50).contains(max_turns) {
                issue(
                    path,
                    "invalid_limit",
                    "max_turns must be between 1 and 50".into(),
                );
            }
            if *transcript_messages > 200 {
                issue(
                    path,
                    "invalid_limit",
                    "transcript_messages must be at most 200".into(),
                );
            }
        }
        EvaluatorSpec::Command {
            command,
            timeout_ms,
        } => {
            if command.trim().is_empty() {
                issue(path, "empty_text", "command cannot be empty".into());
            }
            if !(1..=600_000).contains(timeout_ms) {
                issue(
                    path,
                    "invalid_limit",
                    "timeout_ms must be between 1 and 600000".into(),
                );
            }
        }
    }
}

fn validate_action(
    action: &Action,
    event: RuleEvent,
    path: &str,
    issue: &mut impl FnMut(&str, &'static str, String),
) {
    let non_empty = |issue: &mut dyn FnMut(&str, &'static str, String), text: &str, what: &str| {
        if text.trim().is_empty() {
            issue(path, "empty_text", format!("{what} cannot be empty"));
        }
    };
    match action {
        Action::Inject(inject) => {
            non_empty(issue, &inject.text, "inject text");
            if let Some(placement) = inject.placement {
                if !placement.allowed_on(event) {
                    issue(
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
        Action::Deny { reason } => {
            non_empty(issue, reason, "deny reason");
            if event != RuleEvent::PreToolUse {
                issue(
                    path,
                    "action_not_available",
                    "deny is only available on PreToolUse".into(),
                );
            }
        }
        Action::Ask { .. } | Action::Allow {} => {
            if event != RuleEvent::PreToolUse {
                issue(
                    path,
                    "action_not_available",
                    format!("{} is only available on PreToolUse", action.name()),
                );
            }
        }
        Action::StopRun { reason } => non_empty(issue, reason, "stop_run reason"),
        Action::SwitchProfile { profile } => {
            if profile.id.as_str().trim().is_empty() {
                issue(
                    path,
                    "invalid_profile_reference",
                    "switch target id cannot be empty".into(),
                );
            }
        }
        Action::RewriteArgs { merge } => {
            if event != RuleEvent::PreToolUse {
                issue(
                    path,
                    "action_not_available",
                    "rewrite_args is only available on PreToolUse".into(),
                );
            }
            if !merge.is_object() {
                issue(
                    path,
                    "invalid_merge_patch",
                    "rewrite_args.merge must be a JSON object (RFC 7396 merge patch)".into(),
                );
            }
        }
        Action::RewriteResult { replace, append } => {
            if !matches!(
                event,
                RuleEvent::PostToolUse | RuleEvent::PostToolUseFailure
            ) {
                issue(
                    path,
                    "action_not_available",
                    "rewrite_result is only available on post-tool events".into(),
                );
            }
            if replace.is_some() == append.is_some() {
                issue(
                    path,
                    "invalid_rewrite",
                    "rewrite_result needs exactly one of replace or append".into(),
                );
            }
        }
    }
}

impl CompiledProfile {
    /// Whether the completion gate runs shell commands, which need the
    /// host's explicit trust.
    pub fn uses_commands(&self) -> bool {
        self.profile.completion_gate.as_ref().is_some_and(|gate| {
            gate.checks
                .iter()
                .any(|check| matches!(check.evaluator, Some(EvaluatorSpec::Command { .. })))
        })
    }

    /// Profiles this one can switch to through `switch_profile` rules.
    pub fn switch_targets(&self) -> Vec<ProfileRef> {
        self.profile
            .rules
            .iter()
            .filter_map(|rule| match &rule.action {
                Action::SwitchProfile { profile } => Some(profile.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn reference(&self) -> ProfileRef {
        ProfileRef {
            id: self.profile.id.clone(),
            revision: Some(self.profile.revision),
        }
    }

    /// The system prompt with the profile's instructions applied, choosing a
    /// per-model variant when one matches `model`.
    pub fn system_prompt(&self, base: &str, model: Option<&str>) -> String {
        let instructions = &self.profile.instructions;
        let model = model.map(str::to_lowercase);
        let text = model
            .as_deref()
            .and_then(|model| {
                instructions
                    .variants
                    .iter()
                    .find(|(family, _)| model.contains(family.as_str()))
                    .map(|(_, text)| text.as_str())
            })
            .unwrap_or(&instructions.text);
        match instructions.mode {
            InstructionMode::Replace => text.to_string(),
            InstructionMode::Append if text.is_empty() => base.to_string(),
            InstructionMode::Append if base.is_empty() => text.to_string(),
            InstructionMode::Append => format!("{base}\n\n{text}"),
        }
    }

    /// Whether the profile's scope lets the model see and call `tool`. An
    /// allow-list naming a tool also admits what that tool's grant covers (see
    /// [`crate::tool_alias`]): `write_file` brings `edit_file` with it, and
    /// `read_file` brings `project_info`.
    pub fn allows_tool(&self, tool: &str) -> bool {
        match &self.profile.tools {
            ToolScope::None => false,
            ToolScope::AllowList(tools) => tools
                .iter()
                .any(|allowed| allowed == tool || granted_by(tool) == Some(allowed.as_str())),
            ToolScope::Inherit => true,
        }
    }

    /// The effective permission: the stricter of the session's and the
    /// profile's. A profile can never loosen what the session grants.
    pub fn permission(&self, tool: &str, session: PermissionMode) -> PermissionMode {
        let session_level = match session {
            PermissionMode::Allow => ToolPermission::Allow,
            PermissionMode::Ask => ToolPermission::Ask,
            PermissionMode::Deny => ToolPermission::Deny,
        };
        let override_level = |name: &str| {
            self.profile
                .tool_overrides
                .get(name)
                .and_then(|override_| override_.permission)
        };
        // An override on the tool whose grant covers this one binds it too;
        // when both are set the stricter applies, so naming this tool cannot
        // loosen what the other denies.
        let profile_level = override_level(tool)
            .into_iter()
            .chain(granted_by(tool).and_then(override_level))
            .max()
            .unwrap_or(ToolPermission::Allow);
        match session_level.max(profile_level) {
            ToolPermission::Allow => PermissionMode::Allow,
            ToolPermission::Ask => PermissionMode::Ask,
            ToolPermission::Deny => PermissionMode::Deny,
        }
    }

    pub fn tool_description(&self, tool: &str, description: &str) -> String {
        match self
            .profile
            .tool_overrides
            .get(tool)
            .and_then(|override_| override_.description_append.as_deref())
        {
            Some(extra) if description.is_empty() => extra.to_string(),
            Some(extra) => format!("{description}\n\n{extra}"),
            None => description.to_string(),
        }
    }

    /// The session's execution params with the profile's overlay applied.
    pub fn execution_params(&self, session: &ExecutionParams) -> ExecutionParams {
        let overlay = &self.profile.execution;
        let mut params = session.clone();
        if let Some(model) = &overlay.model {
            params.model = Some(model.clone());
        }
        if let Some(max_tokens) = overlay.max_tokens {
            params.max_tokens = Some(max_tokens);
        }
        if let Some(temperature) = overlay.temperature {
            params.temperature = Some(temperature);
        }
        if let Some(effort) = overlay.reasoning_effort {
            params.reasoning_effort = Some(match effort {
                ReasoningEffortSetting::Minimal => ReasoningEffort::Minimal,
                ReasoningEffortSetting::Low => ReasoningEffort::Low,
                ReasoningEffortSetting::Medium => ReasoningEffort::Medium,
                ReasoningEffortSetting::High => ReasoningEffort::High,
                ReasoningEffortSetting::XHigh => ReasoningEffort::XHigh,
                ReasoningEffortSetting::Max => ReasoningEffort::Max,
                ReasoningEffortSetting::Ultra => ReasoningEffort::Ultra,
            });
        }
        params
    }
}
