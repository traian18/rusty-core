//! Profile documents, execution settings, and declarative rule predicates.
//!
//! Identity, action, and completion-gate models live in focused child modules.
//! Re-exports keep the parent behavior facade stable.

mod actions;
mod completion;
mod identity;

pub use actions::*;
pub use completion::*;
pub use identity::*;

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::orchestration::{DefinitionStatus, ToolScope};

pub const BEHAVIOR_SCHEMA_VERSION: u32 = 1;

/// A behavior profile: what an agent is told, what it may use, and how its
/// loop is bounded. See `BEHAVIOR_LAYER_DESIGN.md` §5.
///
/// Unknown fields are rejected everywhere except `metadata`, which belongs
/// to editors and is preserved but never interpreted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BehaviorProfile {
    pub schema_version: u32,
    pub id: ProfileId,
    pub revision: u64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub status: DefinitionStatus,
    #[serde(default)]
    pub instructions: Instructions,
    /// Tools visible to and executable by the agent, intersected with what
    /// the session grants. Defaults to `inherit`: a profile narrows, never
    /// widens.
    #[serde(default = "inherit_tools")]
    pub tools: ToolScope,
    #[serde(default)]
    pub tool_overrides: BTreeMap<String, ToolOverride>,
    #[serde(default)]
    pub execution: ExecutionOverlay,
    #[serde(default)]
    pub limits: Limits,
    /// Loop rules: on an event, when a condition holds, do an action.
    /// Evaluated in order (`BEHAVIOR_LAYER_DESIGN.md` §6).
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// Checks a proposed final answer must pass before the run may finish
    /// (`BEHAVIOR_LAYER_DESIGN.md` §7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_gate: Option<CompletionGate>,
    #[serde(default)]
    pub children: ChildPolicy,
    #[serde(default)]
    pub metadata: Value,
}

fn inherit_tools() -> ToolScope {
    ToolScope::Inherit
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Instructions {
    #[serde(default)]
    pub mode: InstructionMode,
    #[serde(default)]
    pub text: String,
    /// Per-model-family wording, keyed by a lowercase substring of the model
    /// id (e.g. `"claude"`, `"gpt"`, `"gemini"`). The first key, in sorted
    /// order, contained in the active model id replaces `text`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variants: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstructionMode {
    /// Added after the agent's own system prompt.
    #[default]
    Append,
    /// Replaces the agent's own system prompt.
    Replace,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolOverride {
    /// Tightens the session's policy for this tool; it can never loosen it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<ToolPermission>,
    /// Appended to the tool's description as sent to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_append: Option<String>,
}

/// Ordered from least to most restrictive.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermission {
    Allow,
    Ask,
    Deny,
}

/// Execution parameters applied over the session's while the profile is
/// active. Unset fields keep the session's value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutionOverlay {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortSetting>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffortSetting {
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
    Ultra,
}

/// Per-run bounds on the agent loop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Maximum model requests per run. The last one is sent with no tools
    /// and `final_turn_prompt`, so the model must answer in text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// Maximum tool calls per run. Reaching it makes the next request the
    /// final turn; calls beyond it are denied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_turn_prompt: Option<String>,
}

/// How spawned child agents get their behavior (`BEHAVIOR_LAYER_DESIGN.md`
/// §8a). Only `inherit` is implemented; the other types are reserved so the
/// format stays stable, and the compiler rejects them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChildPolicy {
    #[default]
    Inherit,
    Named {
        profile: ProfileRef,
    },
    Derived {
        #[serde(default)]
        overrides: Value,
    },
    Policy {
        #[serde(rename = "ref")]
        reference: ProfileRef,
    },
}

impl ChildPolicy {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Named { .. } => "named",
            Self::Derived { .. } => "derived",
            Self::Policy { .. } => "policy",
        }
    }
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

/// `on <event> [when <condition>] do <action>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Stable identifier, reported in events and used by editors.
    pub id: String,
    pub on: RuleEvent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Condition>,
    #[serde(rename = "do")]
    pub action: Action,
    /// Maximum firings per run. Unlimited when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_fires: Option<u32>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

/// Loop events, named after the Claude Code / Codex hook events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RuleEvent {
    /// A run starts, after the user's input is recorded.
    RunStart,
    /// Before every new model request (not re-issues after a pause).
    BeforeModelRequest,
    /// The model requested a tool, before permission and execution.
    PreToolUse,
    /// An executed tool succeeded.
    PostToolUse,
    /// An executed tool failed or reported an error.
    PostToolUseFailure,
    /// This profile became active by a switch (rule, host, or a new run
    /// under a newly set profile). Fires before the first request under it.
    ProfileEntered,
}

impl RuleEvent {
    pub fn name(self) -> &'static str {
        match self {
            Self::RunStart => "RunStart",
            Self::BeforeModelRequest => "BeforeModelRequest",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::ProfileEntered => "ProfileEntered",
        }
    }

    pub fn is_tool_event(self) -> bool {
        matches!(
            self,
            Self::PreToolUse | Self::PostToolUse | Self::PostToolUseFailure
        )
    }
}

/// A declarative, side-effect-free predicate over the loop's state and the
/// event. There are no expressions and no regular expressions; globs use
/// `*` (within a path segment), `**` (across segments) and `?`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    /// The event's tool name matches (tool events only).
    Tool(NameMatch),
    /// A value inside the tool arguments matches (tool events only).
    Arg(ArgMatch),
    /// The tool result text contains this string (post-tool events only).
    ResultContains(String),
    /// The run's current turn number (1-based).
    Turn(Comparison),
    /// Executed calls of a tool in this run. `outcome` narrows it to calls
    /// that succeeded or failed.
    Calls(ToolCount),
    /// Turns since the tool was last executed (or since the run started).
    /// `outcome` narrows it to the last call that succeeded or failed.
    TurnsSinceCall(ToolCount),
    /// Executed calls of `called` since the last executed call of `of`.
    /// False when `of` has not been called in this run. `outcome` narrows
    /// the counted `called` calls to those that succeeded or failed.
    SinceLastCall(SinceLastCall),
    /// A tool matching this was offered to the model on its latest request.
    /// Lets a gate skip a requirement the model had no way to meet, such as
    /// "run a check" in a session that was given no way to run one.
    ToolOffered(NameMatch),
    /// Consecutive requests of the same tool with identical arguments,
    /// including the current one (`PreToolUse` only).
    RepeatedCall(Comparison),
    /// The id of the profile active before this one (`ProfileEntered` only).
    ProfileEnteredFrom(NameMatch),
    /// The status the model gave in its latest proposed final answer: the
    /// value of its last `Status:` line (or the line under a `Status`
    /// heading), lower-cased, without Markdown emphasis, cut at the first
    /// separator (`—`, `–`, ` - `, `:`, `;`, `,`, `.`, `(`). So
    /// `**Status:** Not done — tests remain` is `not done`. An answer without
    /// a status line has the empty status, which no pattern matches. False
    /// until the model has proposed a final answer in this run, so it is
    /// meant for completion gates.
    FinalStatus(NameMatch),
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
}

/// One tool-name glob, or a list of them (any may match).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum NameMatch {
    One(String),
    Any(Vec<String>),
}

impl NameMatch {
    pub fn patterns(&self) -> &[String] {
        match self {
            Self::One(pattern) => std::slice::from_ref(pattern),
            Self::Any(patterns) => patterns,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArgMatch {
    /// JSON Pointer into the tool arguments (`""` is the whole value).
    pub pointer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Value>,
}

/// Every bound given must hold.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Comparison {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gte: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lte: Option<u32>,
}

impl Comparison {
    pub fn is_empty(&self) -> bool {
        self.eq.is_none() && self.gte.is_none() && self.lte.is_none()
    }

    pub fn holds(&self, value: u32) -> bool {
        self.eq.is_none_or(|eq| value == eq)
            && self.gte.is_none_or(|gte| value >= gte)
            && self.lte.is_none_or(|lte| value <= lte)
    }
}

/// Which executed calls a count looks at. A call *failed* when its result was
/// an error, so a tool that reports a failing check as an error (the IDE's
/// `run_check`) is counted as failed, and one that passed as succeeded. A
/// tool that returns text for a failed command with no error flag (such as
/// `run_command`) always counts as succeeded: the engine cannot see an exit
/// code inside its output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CallOutcome {
    /// Every executed call.
    #[default]
    Any,
    Succeeded,
    Failed,
}

impl CallOutcome {
    pub fn is_any(&self) -> bool {
        matches!(self, Self::Any)
    }

    /// Whether a call that `failed` (or not) is one this filter counts.
    pub fn admits(self, failed: bool) -> bool {
        match self {
            Self::Any => true,
            Self::Succeeded => !failed,
            Self::Failed => failed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolCount {
    pub tool: NameMatch,
    #[serde(default, skip_serializing_if = "CallOutcome::is_any")]
    pub outcome: CallOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gte: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lte: Option<u32>,
}

impl ToolCount {
    pub fn comparison(&self) -> Comparison {
        Comparison {
            eq: self.eq,
            gte: self.gte,
            lte: self.lte,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SinceLastCall {
    pub of: NameMatch,
    pub called: NameMatch,
    /// Which of the `called` calls count. `of` always matches any outcome.
    #[serde(default, skip_serializing_if = "CallOutcome::is_any")]
    pub outcome: CallOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gte: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lte: Option<u32>,
}

impl SinceLastCall {
    pub fn comparison(&self) -> Comparison {
        Comparison {
            eq: self.eq,
            gte: self.gte,
            lte: self.lte,
        }
    }
}
