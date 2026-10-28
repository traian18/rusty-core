//! Completion gates and evaluator configuration.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Condition;

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
    /// When the gate has no continuations left, stop holding the run back
    /// on this check: if only waivable checks still fail, the run completes
    /// as passed and the event lists them in `failed_checks`. For checks
    /// that ask the model to keep working rather than guard correctness.
    #[serde(default, skip_serializing_if = "is_false")]
    pub waive_when_exhausted: bool,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

fn is_false(value: &bool) -> bool {
    !*value
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
        /// The argument that receives the proposed answer, for a check of
        /// the answer itself (a task list against its spec) rather than of
        /// the workspace.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer_arg: Option<String>,
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
