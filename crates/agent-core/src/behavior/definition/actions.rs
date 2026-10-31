//! Declarative actions and injection placement.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ProfileRef, RuleEvent};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Add context for the model (guidance).
    Inject(Inject),
    /// Refuse the tool call; the model receives `reason` as the result
    /// (enforced, `PreToolUse` only).
    Deny { reason: String },
    /// Require approval for this call even if policy allows it
    /// (enforced, `PreToolUse` only).
    Ask {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Skip the approval prompt for this call. Applies only where the
    /// effective permission is `ask`; never overrides a denial
    /// (enforced, `PreToolUse` only).
    Allow {},
    /// Fail the run with `BEHAVIOR_STOP` (enforced).
    StopRun { reason: String },
    /// Switch to another profile from the next model request (enforced for
    /// tools, parameters and limits; the model is told about the switch).
    /// The target must be resolvable when this profile is installed.
    SwitchProfile { profile: ProfileRef },
    /// JSON merge patch (RFC 7396) applied to the tool arguments before
    /// the call is approved or executed (enforced, `PreToolUse` only). The
    /// model is told the arguments were adjusted.
    RewriteArgs { merge: Value },
    /// Change what the model receives as the tool result (enforced,
    /// post-tool events only). Exactly one of `replace` or `append`.
    RewriteResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        append: Option<String>,
    },
}

impl Action {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Inject(_) => "inject",
            Self::Deny { .. } => "deny",
            Self::Ask { .. } => "ask",
            Self::Allow {} => "allow",
            Self::StopRun { .. } => "stop_run",
            Self::SwitchProfile { .. } => "switch_profile",
            Self::RewriteArgs { .. } => "rewrite_args",
            Self::RewriteResult { .. } => "rewrite_result",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Inject {
    pub text: String,
    /// Where the text goes. Defaults by event: `with_result` for tool
    /// events, `next_request` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<Placement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// Appended to the tool call's result; stored in the transcript.
    WithResult,
    /// Appended to the next outgoing request only; not stored.
    NextRequest,
    /// Appended to the run's user message; stored (`RunStart` only).
    Persistent,
}

impl Placement {
    pub fn name(self) -> &'static str {
        match self {
            Self::WithResult => "with_result",
            Self::NextRequest => "next_request",
            Self::Persistent => "persistent",
        }
    }

    pub fn default_for(event: RuleEvent) -> Self {
        if event.is_tool_event() {
            Self::WithResult
        } else {
            Self::NextRequest
        }
    }

    pub fn allowed_on(self, event: RuleEvent) -> bool {
        match self {
            Self::WithResult => event.is_tool_event(),
            Self::NextRequest => true,
            Self::Persistent => event == RuleEvent::RunStart,
        }
    }
}
