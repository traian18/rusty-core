//! What a compiled profile answers: its prompt, tool scope, permissions and execution settings.

use rusty_protocol::{
    backend::{ExecutionParams, ReasoningEffort},
    tools::PermissionMode,
};

use super::CompiledProfile;
use crate::{
    behavior::{
        definition::{
            Action, EvaluatorSpec, InstructionMode, ProfileRef, ReasoningEffortSetting,
            ToolPermission,
        },
        ToolScope,
    },
    tool_alias::{granted_by, ALWAYS_ADMITTED},
};

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
    /// `read_file` brings `project_info` and `repo_context`. Any allow-list at
    /// all admits the repository tools every model gets ([`ALWAYS_ADMITTED`]).
    pub fn allows_tool(&self, tool: &str) -> bool {
        match &self.profile.tools {
            ToolScope::None => false,
            ToolScope::AllowList(tools) if !tools.is_empty() && ALWAYS_ADMITTED.contains(&tool) => {
                true
            }
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
