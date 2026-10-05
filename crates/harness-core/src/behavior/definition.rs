use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::orchestration::{DefinitionStatus, ToolScope};

pub const BEHAVIOR_SCHEMA_VERSION: u32 = 1;

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ProfileId(pub String);

impl ProfileId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ProfileId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl std::fmt::Display for ProfileId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Reference to a profile revision. Without a revision it means the highest
/// published revision of `id` (or the newest draft, where drafts are allowed).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct ProfileRef {
    pub id: ProfileId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

impl ProfileRef {
    pub fn exact(id: impl Into<String>, revision: u64) -> Self {
        Self {
            id: ProfileId::new(id),
            revision: Some(revision),
        }
    }

    pub fn latest(id: impl Into<String>) -> Self {
        Self {
            id: ProfileId::new(id),
            revision: None,
        }
    }
}

impl std::fmt::Display for ProfileRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.revision {
            Some(revision) => write!(formatter, "{}@{revision}", self.id),
            None => write!(formatter, "{}@latest", self.id),
        }
    }
}

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
        self.eq.map_or(true, |eq| value == eq)
            && self.gte.map_or(true, |gte| value >= gte)
            && self.lte.map_or(true, |lte| value <= lte)
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

// ---------------------------------------------------------------------------
// Completion gate
// ---------------------------------------------------------------------------

/// When the model proposes a final answer (a turn with no tool calls), every
/// check runs. If any fails, their feedback is sent back as a user message
/// and the loop continues, up to `max_continuations` times.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompletionGate {
    pub checks: Vec<GateCheck>,
    #[serde(default = "default_max_continuations")]
    pub max_continuations: u32,
    /// What happens when checks still fail after the last continuation.
    /// Defaults to `accept`: the run completes and the
    /// `CompletionGateEvaluated` event reports `passed: false`. Workflow
    /// steps treat an unpassed gate as a failed attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_exhausted: Option<OnExhausted>,
}

fn default_max_continuations() -> u32 {
    3
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnExhausted {
    /// Fail the run with `GATE_EXHAUSTED`.
    Fail,
    /// Complete the run, reported as not passed.
    Accept,
}

/// One check: either a declarative `require` condition (evaluated
/// instantly, no I/O) or an `evaluator` (runs a tool or asks a model).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GateCheck {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require: Option<Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator: Option<EvaluatorSpec>,
    /// Sent to the model when the check fails. For evaluators, defaults to
    /// the evaluator's own feedback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    /// How an evaluator that cannot produce a verdict counts.
    #[serde(default)]
    pub error_policy: ErrorPolicy,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorPolicy {
    #[default]
    Fail,
    Pass,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvaluatorSpec {
    /// Run a registered tool directly. Passes when it succeeds; its output
    /// becomes the feedback. The session must allow the tool without
    /// approval. The profile's tool scope does not apply: a gate may use a
    /// tool the model cannot see.
    Tool {
        tool: String,
        #[serde(default)]
        args: Value,
    },
    /// Ask a model whether the answer is acceptable. It returns
    /// `{"passed": bool, "feedback": string}`, validated by the host.
    Model {
        instructions: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// How many recent transcript messages the evaluator sees.
        #[serde(default = "default_transcript_messages")]
        transcript_messages: u32,
    },
    /// An isolated verifier agent that can investigate with its own tools
    /// before answering `{"passed": bool, "feedback": string}`. Its tools
    /// must be allowed by the session without approval.
    Agent {
        instructions: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tools: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default = "default_verifier_turns")]
        max_turns: u32,
        #[serde(default = "default_transcript_messages")]
        transcript_messages: u32,
    },
    /// A shell command following the Claude Code / Codex `Stop` hook
    /// contract: JSON on stdin; exit 0 passes (unless stdout is
    /// `{"decision": "block", "reason": ...}`), exit 2 fails with stderr as
    /// feedback, anything else is an error. Runs only when the host trusts
    /// the profile to run commands.
    Command {
        command: String,
        #[serde(default = "default_command_timeout_ms")]
        timeout_ms: u64,
    },
}

fn default_verifier_turns() -> u32 {
    8
}

fn default_command_timeout_ms() -> u64 {
    60_000
}

fn default_transcript_messages() -> u32 {
    10
}

impl EvaluatorSpec {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Tool { .. } => "tool",
            Self::Model { .. } => "model",
            Self::Agent { .. } => "agent",
            Self::Command { .. } => "command",
        }
    }
}
