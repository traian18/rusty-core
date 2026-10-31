//! The checks a requested tool call must pass, in order. Each returns the effects that refuse the
//! call when it does not pass.

use rusty_protocol::{
    effects::AgentEffect,
    ids::ToolCallId,
    tools::{PermissionMode, ToolCall, ToolError},
};

use super::Approval;
use crate::{
    agent::Agent,
    behavior::{RuleEvent, RuleOutcome, ToolDecision, ToolEvent},
};

/// What the checks produce: the call to run, or the effects that refuse it.
type Admission<T> = Result<T, Vec<AgentEffect>>;

impl Agent {
    /// Runs every check. Effects the rules produce along the way go into `effects`, whether or
    /// not the call is finally admitted.
    pub(super) fn admit_tool_call(
        &mut self,
        call: ToolCall,
        effects: &mut Vec<AgentEffect>,
    ) -> Admission<(ToolCall, Approval)> {
        let session_mode = self.session_permission(&call)?;
        self.require_profile_scope(&call)?;
        let outcome = self.evaluate_request_rules(&call, effects)?;
        let approval = self.approval_for(&call, session_mode, &outcome)?;
        let call = self.rewrite_arguments(call, &outcome);
        self.require_tool_budget(call.id)?;
        Ok((call, approval))
    }

    /// What the session grants for this tool. A tool the session does not offer, or has disabled,
    /// is a failed call.
    fn session_permission(&mut self, call: &ToolCall) -> Admission<PermissionMode> {
        let granted = self
            .capabilities
            .tools
            .tools
            .values()
            .find(|capability| capability.descriptor.name == call.name && capability.policy.enabled)
            .map(|capability| capability.policy.permission.clone());
        granted.ok_or_else(|| self.tool_failed(call.id, ToolError::PermissionDenied))
    }

    /// The profile narrows what the session grants: a tool outside its scope cannot run even if
    /// the model names it.
    fn require_profile_scope(&mut self, call: &ToolCall) -> Admission<()> {
        let profile = self.state.behavior.profile.clone();
        if profile.allows_tool(&call.name) {
            return Ok(());
        }
        let reason = format!(
            "Tool {} is not available under profile {}.",
            call.name,
            profile.reference()
        );
        Err(self.deny_tool(call.id, None, reason))
    }

    /// Lets the profile's rules see the request. A rule that stops the run refuses the call.
    fn evaluate_request_rules(
        &mut self,
        call: &ToolCall,
        effects: &mut Vec<AgentEffect>,
    ) -> Admission<RuleOutcome> {
        self.state.behavior.note_tool_request(call);
        let outcome = self.state.behavior.evaluate(
            RuleEvent::PreToolUse,
            Some(ToolEvent { call, result: None }),
        );
        effects.extend(self.apply_rule_outcome(&outcome, Some(call.id)));
        match &outcome.stop {
            None => Ok(outcome),
            Some(stop) => {
                let mut refusal =
                    self.deny_tool(call.id, Some(stop.rule_id.clone()), stop.reason.clone());
                refusal.extend(self.behavior_stop(stop));
                Err(refusal)
            }
        }
    }

    /// Combines what the session, the profile and the rules say. A rule can tighten the
    /// permission to `Ask` or deny the call, and can relax `Ask` to `Allow`, but cannot lift a
    /// denial.
    fn approval_for(
        &mut self,
        call: &ToolCall,
        session_mode: PermissionMode,
        outcome: &RuleOutcome,
    ) -> Admission<Approval> {
        let profile = self.state.behavior.profile.clone();
        let session_denies = matches!(session_mode, PermissionMode::Deny);
        let mut mode = profile.permission(&call.name, session_mode);
        match &outcome.decision {
            Some(ToolDecision::Deny { rule_id, reason }) => {
                return Err(self.deny_tool(call.id, Some(rule_id.clone()), reason.clone()));
            }
            Some(ToolDecision::Ask { .. }) if !matches!(mode, PermissionMode::Deny) => {
                mode = PermissionMode::Ask;
            }
            Some(ToolDecision::Allow { .. }) if matches!(mode, PermissionMode::Ask) => {
                mode = PermissionMode::Allow;
            }
            _ => {}
        }
        match mode {
            PermissionMode::Allow => Ok(Approval::Allow),
            PermissionMode::Ask => Ok(Approval::Ask),
            PermissionMode::Deny if session_denies => {
                Err(self.tool_failed(call.id, ToolError::PermissionDenied))
            }
            PermissionMode::Deny => {
                let reason = format!(
                    "Tool {} is denied by profile {}.",
                    call.name,
                    profile.reference()
                );
                Err(self.deny_tool(call.id, None, reason))
            }
        }
    }

    /// Calls past the run's tool budget, or on the final turn, are refused.
    fn require_tool_budget(&mut self, call_id: ToolCallId) -> Admission<()> {
        if self.state.behavior.admit_tool_call() {
            return Ok(());
        }
        let reason = if self.state.behavior.run.final_turn {
            "No tools are available on the final turn. Answer in text."
        } else {
            "The tool-call budget for this run is exhausted. Answer with what you have."
        };
        Err(self.deny_tool(call_id, None, reason.to_string()))
    }
}
