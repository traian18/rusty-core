use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ORCHESTRATION_SCHEMA_VERSION: u32 = 1;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

string_id!(OrchestrationDefinitionId);
string_id!(OrchestrationNodeId);
string_id!(OrchestrationEdgeId);
string_id!(OrchestrationRunId);

/// Publication state of a definition revision.
///
/// Published revisions are immutable; drafts only execute when the registry
/// explicitly allows them (development mode).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DefinitionStatus {
    Draft,
    #[default]
    Published,
    Deprecated,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationDefinition {
    pub schema_version: u32,
    pub id: OrchestrationDefinitionId,
    pub revision: u64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub status: DefinitionStatus,
    #[serde(default)]
    pub input_schema: Option<SchemaReference>,
    /// Optional: by default the result is the text of the output node.
    #[serde(default)]
    pub output_contract: OutputContract,
    pub nodes: Vec<OrchestrationNode>,
    pub edges: Vec<OrchestrationEdge>,
    #[serde(default)]
    pub policies: OrchestrationPolicies,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationNode {
    pub id: OrchestrationNodeId,
    pub name: String,
    #[serde(flatten)]
    pub kind: OrchestrationNodeKind,
    #[serde(default)]
    pub input_bindings: Vec<InputBinding>,
    #[serde(default)]
    pub output_schema: Option<SchemaReference>,
    #[serde(default)]
    pub retry: RetryPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "config", rename_all = "snake_case")]
pub enum OrchestrationNodeKind {
    Input(InputNodeConfig),
    Agent(AgentNodeConfig),
    Verify(VerifyNodeConfig),
    Output(OutputNodeConfig),
}

impl OrchestrationNodeKind {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Input(_) => "input",
            Self::Agent(_) => "agent",
            Self::Verify(_) => "verify",
            Self::Output(_) => "output",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InputNodeConfig {
    #[serde(default)]
    pub defaults: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskQueueConfig {
    /// Pointer to a typed requirement/task plan in the bound input.
    pub plan_pointer: String,
    pub review_profile: crate::behavior::ProfileRef,
    pub review_instructions: String,
    pub max_repairs: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentNodeConfig {
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_queue: Option<TaskQueueConfig>,
    /// Tools visible to and executable by this step. Missing means `none`:
    /// a step never gains tools it did not declare.
    #[serde(default)]
    pub tools: ToolScope,
    #[serde(default)]
    pub context_mode: AgentContextMode,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub structured_output: StructuredOutputMode,
    /// Behavior profile for the agent executing this step. Without one the
    /// step runs under the parent session's active profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<crate::behavior::ProfileRef>,
}

/// Which tools an agent step may see and execute.
///
/// There is deliberately no deny list: a tool registered after the definition
/// was written must never become available implicitly. `Inherit` is resolved
/// into an explicit allow list at run start and recorded on every attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "tools", rename_all = "snake_case")]
pub enum ToolScope {
    #[default]
    None,
    AllowList(Vec<String>),
    Inherit,
}

/// How a step's output schema is enforced. Host-side validation always runs,
/// whichever mode is chosen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredOutputMode {
    /// Forward the final message verbatim; no model-facing schema or JSON parsing.
    Text,
    /// Reject the step before any model request if the backend lacks
    /// native structured output.
    #[default]
    Require,
    /// Use native structured output when the backend advertises it; when it
    /// does not, ask for JSON text and rely on host-side validation.
    HostValidatedFallback,
    /// Never send a native schema, even to a backend that advertises support:
    /// put the schema in the prompt, ask for JSON text, and rely on host-side
    /// validation (with the step's retries). A backend's capability is a
    /// property of the whole integration, not of the model behind it, and
    /// some models cannot combine tool calls with a constrained response, or
    /// reject a schema their constraint compiler finds too large. Every
    /// request of a tool-using step carries the constraint, so those failures
    /// are hard errors on the first request.
    HostValidated,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentContextMode {
    SharedSession,
    #[default]
    IsolatedChild,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifyNodeConfig {
    #[serde(default)]
    pub checks: Vec<VerificationCheck>,
    /// Upstream node to re-run when verification fails with a retryable
    /// reason. This is how `Verify → Execute` is expressed without a graph
    /// cycle: it is bounded by the target's own retry policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_target: Option<OrchestrationNodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VerificationCheck {
    /// Every expected acceptance criterion has exactly one passing, evidenced result.
    RequirementsSatisfied {
        plan_pointer: String,
        results_pointer: String,
    },
    /// Every bound upstream output conforms to its producer's output schema.
    Schema,
    /// The string at `pointer` equals `equals`.
    RequiredStatus { pointer: String, equals: String },
    /// The value at `pointer` is present and non-empty.
    ArtifactExists { pointer: String },
    /// Every `{kind, reference}` entry in the array at `pointer` resolves
    /// through the host's artifact resolver (e.g. files exist in the
    /// workspace), rather than trusting the model's claim.
    ArtifactsResolvable { pointer: String },
}

impl VerificationCheck {
    pub fn id(&self) -> String {
        match self {
            Self::RequirementsSatisfied { .. } => "requirements_satisfied".into(),
            Self::Schema => "schema".into(),
            Self::RequiredStatus { pointer, .. } => format!("required_status:{pointer}"),
            Self::ArtifactExists { pointer } => format!("artifact_exists:{pointer}"),
            Self::ArtifactsResolvable { pointer } => format!("artifacts_resolvable:{pointer}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputNodeConfig {
    pub source: OutputBinding,
    #[serde(default = "default_true")]
    pub strict: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationEdge {
    pub id: OrchestrationEdgeId,
    pub source: OrchestrationNodeId,
    pub target: OrchestrationNodeId,
    pub condition: EdgeCondition,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeCondition {
    OnSuccess,
    OnFailure,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputBinding {
    pub target: String,
    pub source: OutputBinding,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputBinding {
    RunInput {
        pointer: String,
    },
    NodeOutput {
        node_id: OrchestrationNodeId,
        pointer: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputContract {
    pub schema: SchemaReference,
    /// Which node produces the result; omitted, it is the output node's own
    /// source (the usual case).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<OutputBinding>,
    #[serde(default = "default_true")]
    pub strict: bool,
}

impl Default for OutputContract {
    /// A written result, taken from the output node and not schema-checked.
    fn default() -> Self {
        Self {
            schema: SchemaReference::Inline {
                name: "result".into(),
                schema: serde_json::json!({"type": "string"}),
            },
            source: None,
            strict: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SchemaReference {
    Inline { name: String, schema: Value },
    Registry { schema_id: String, revision: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    #[serde(default)]
    pub retry_on: Vec<RetryReason>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            retry_on: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryReason {
    BackendRateLimited,
    BackendTimeout,
    ToolTimeout,
    InvalidStructuredOutput,
    VerificationFailed,
}

/// Upper bound on any node's `retry.max_attempts`.
pub const MAX_STEP_ATTEMPTS: u32 = 10;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OrchestrationPolicies {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_attempts: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_elapsed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_model_requests: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
}

const fn default_true() -> bool {
    true
}
