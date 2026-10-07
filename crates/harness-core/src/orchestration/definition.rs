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
    Approval(ApprovalNodeConfig),
    Subflow(SubflowNodeConfig),
    Output(OutputNodeConfig),
}

impl OrchestrationNodeKind {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Input(_) => "input",
            Self::Agent(_) => "agent",
            Self::Verify(_) => "verify",
            Self::Approval(_) => "approval",
            Self::Subflow(_) => "subflow",
            Self::Output(_) => "output",
        }
    }
}

/// Runs another flow, or a single step, as part of this one: to gather
/// extra information with research, fix a bug found on the way, and so on.
/// The node's input bindings become the child's run input and the child's
/// result becomes the node's output. The child runs with the same tools,
/// model and permissions as this run; its permission requests and questions
/// to the user surface here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubflowNodeConfig {
    pub target: SubflowTarget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SubflowTarget {
    /// A saved flow, by definition id (the latest revision unless one is given).
    Flow {
        id: OrchestrationDefinitionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<u64>,
    },
    /// One step: an agent following `instructions` under `profile`, run as a
    /// flow of its own. Everything bound to the node is its input.
    Step {
        instructions: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<crate::behavior::ProfileRef>,
        #[serde(default = "inherit_tools")]
        tools: ToolScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
}

const fn inherit_tools() -> ToolScope {
    ToolScope::Inherit
}

/// How deeply flows may call flows: a flow run on its own is level 0.
pub const MAX_SUBFLOW_DEPTH: u32 = 3;

impl SubflowTarget {
    /// The one-step flow a `Step` target runs, with `inputs` as the names of
    /// the values bound to the calling node. `None` for a `Flow` target.
    pub fn step_definition(
        &self,
        node: &OrchestrationNodeId,
        inputs: &[String],
    ) -> Option<OrchestrationDefinition> {
        let Self::Step {
            instructions,
            profile,
            tools,
            model,
        } = self
        else {
            return None;
        };
        let id = OrchestrationNodeId::from("step");
        let input = OrchestrationNodeId::from("input");
        let output = OrchestrationNodeId::from("output");
        let node_input = |name: &str| InputBinding {
            target: name.to_owned(),
            source: OutputBinding::RunInput {
                pointer: format!("/{name}"),
            },
        };
        let edge = |source: &OrchestrationNodeId, target: &OrchestrationNodeId| OrchestrationEdge {
            id: OrchestrationEdgeId::new(format!("{source}-{target}")),
            source: source.clone(),
            target: target.clone(),
            condition: EdgeCondition::OnSuccess,
            metadata: Value::Null,
        };
        let plain = |id: &OrchestrationNodeId, name: &str, kind| OrchestrationNode {
            id: id.clone(),
            name: name.into(),
            kind,
            input_bindings: Vec::new(),
            output_schema: None,
            retry: RetryPolicy::default(),
            timeout_ms: None,
            metadata: Value::Null,
        };
        let mut agent = plain(
            &id,
            "Step",
            OrchestrationNodeKind::Agent(AgentNodeConfig {
                instructions: instructions.clone(),
                task_queue: None,
                tools: tools.clone(),
                context_mode: AgentContextMode::IsolatedChild,
                model: model.clone(),
                structured_output: StructuredOutputMode::Text,
                profile: profile.clone(),
            }),
        );
        agent.input_bindings = inputs.iter().map(|name| node_input(name)).collect();
        agent.output_schema = Some(SchemaReference::Inline {
            name: "step".into(),
            schema: serde_json::json!({"type": "string"}),
        });
        Some(OrchestrationDefinition {
            schema_version: ORCHESTRATION_SCHEMA_VERSION,
            id: OrchestrationDefinitionId::new(format!("subflow.step.{node}")),
            revision: 1,
            name: format!("Step {node}"),
            description: None,
            status: DefinitionStatus::Published,
            input_schema: None,
            output_contract: OutputContract::default(),
            nodes: vec![
                plain(
                    &input,
                    "Input",
                    OrchestrationNodeKind::Input(InputNodeConfig::default()),
                ),
                agent,
                plain(
                    &output,
                    "Output",
                    OrchestrationNodeKind::Output(OutputNodeConfig {
                        source: OutputBinding::NodeOutput {
                            node_id: id.clone(),
                            pointer: String::new(),
                        },
                        strict: false,
                    }),
                ),
            ],
            edges: vec![edge(&input, &id), edge(&id, &output)],
            policies: OrchestrationPolicies::default(),
            metadata: Value::Null,
        })
    }
}

/// Pauses the run until the user reviews `subject` and approves it, asks for
/// changes, or rejects it. Requested changes go back to `revise_target` with
/// the user's notes, the same way a failed verification goes back to its
/// `retry_target`; the steps in between run again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalNodeConfig {
    /// What the user reviews, usually the output of the step before.
    pub subject: OutputBinding,
    /// Shown above the subject; empty uses a generic question.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    /// The upstream agent step that requested changes go back to. Omitted,
    /// it is the step that produced the subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revise_target: Option<OrchestrationNodeId>,
    /// How many times the user may ask for changes; after that the step only
    /// offers approve or reject.
    #[serde(default = "default_max_revisions")]
    pub max_revisions: u32,
    /// Whether a run started with auto-approve passes this step without
    /// asking. A step that must always be seen by a person sets it false.
    #[serde(default = "default_true")]
    pub allow_auto_approve: bool,
    /// Pass without asking when the subject has nothing at this pointer
    /// (missing, null, or an empty string, array or object).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_if_empty: Option<String>,
}

impl ApprovalNodeConfig {
    /// Where requested changes go: the explicit target, else the subject's producer.
    pub fn revise_target(&self) -> Option<&OrchestrationNodeId> {
        self.revise_target.as_ref().or(match &self.subject {
            OutputBinding::NodeOutput { node_id, .. } => Some(node_id),
            OutputBinding::RunInput { .. } => None,
        })
    }
}

/// Upper bound on an approval's `max_revisions`.
pub const MAX_REVISIONS: u32 = 10;

const fn default_max_revisions() -> u32 {
    5
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InputNodeConfig {
    #[serde(default)]
    pub defaults: BTreeMap<String, Value>,
}

/// Runs a plan's tasks one at a time, each in a fresh builder session
/// followed by a read-only review, checkpointing every accepted task. The
/// step's own output is the list of completed tasks (always an object,
/// whatever the step's response format).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskQueueConfig {
    /// Pointer to a typed requirement/task plan in the bound input; its first
    /// segment names the input binding the plan comes from.
    pub plan_pointer: String,
    pub review_profile: crate::behavior::ProfileRef,
    pub review_instructions: String,
    pub max_repairs: u32,
    /// What happens when a task cannot be completed: its repairs ran out, or
    /// it needs something only the user can give.
    #[serde(default)]
    pub on_task_failure: TaskFailurePolicy,
    /// Named flows a planned task may run instead of the builder and
    /// reviewer, by setting its `flow` to the name: gathering more
    /// information by research, fixing a bug found on the way, and so on. A
    /// flow judges its own result; a failed one fails the task.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub flows: BTreeMap<String, SubflowTarget>,
}

impl TaskQueueConfig {
    /// The input binding the plan is read from.
    pub fn plan_binding(&self) -> Option<&str> {
        self.plan_pointer
            .strip_prefix('/')?
            .split('/')
            .next()
            .filter(|segment| !segment.is_empty())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskFailurePolicy {
    /// Ask the user: try again (with their notes), skip the task, revise the
    /// plan, or stop.
    #[default]
    Ask,
    /// End the step; completed tasks stay checkpointed for a continuation.
    Stop,
    /// Record the task as skipped and go on with the next one.
    Skip,
}

impl OrchestrationNode {
    /// The step that produced a task queue's plan, when the plan is bound
    /// from a step's output. Asking to revise the plan re-runs that step.
    pub fn task_plan_source(&self) -> Option<&OrchestrationNodeId> {
        let OrchestrationNodeKind::Agent(AgentNodeConfig {
            task_queue: Some(queue),
            ..
        }) = &self.kind
        else {
            return None;
        };
        let target = queue.plan_binding()?;
        self.input_bindings
            .iter()
            .find(|binding| binding.target == target)
            .and_then(|binding| match &binding.source {
                OutputBinding::NodeOutput { node_id, .. } => Some(node_id),
                OutputBinding::RunInput { .. } => None,
            })
    }
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
    /// Judges the `{id, status, evidence, how_to_test?}` array at `pointer`
    /// criterion by criterion instead of trusting a free-text verdict. Passes
    /// when it is non-empty and no criterion has a `block_on` status. A
    /// `defer` status (by default `manual`: only a person can check it) lets
    /// the work move on and is reported as a manual check for the user, never
    /// sent back to the producer. Any other non-`pass` status blocks. With
    /// `plan_pointer`, every acceptance criterion of that task plan must be
    /// answered too; one nobody reported blocks as `missing`.
    Criteria {
        pointer: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_pointer: Option<String>,
        #[serde(default = "default_block_on")]
        block_on: Vec<String>,
        #[serde(default = "default_defer")]
        defer: Vec<String>,
        /// More deferred criteria than this blocks; omitted, any number may be deferred.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_deferred: Option<u32>,
    },
}

fn default_block_on() -> Vec<String> {
    vec!["fail".into()]
}

fn default_defer() -> Vec<String> {
    vec!["manual".into()]
}

impl VerificationCheck {
    pub fn id(&self) -> String {
        match self {
            Self::RequirementsSatisfied { .. } => "requirements_satisfied".into(),
            Self::Schema => "schema".into(),
            Self::RequiredStatus { pointer, .. } => format!("required_status:{pointer}"),
            Self::ArtifactExists { pointer } => format!("artifact_exists:{pointer}"),
            Self::ArtifactsResolvable { pointer } => format!("artifacts_resolvable:{pointer}"),
            Self::Criteria { pointer, .. } => format!("criteria:{pointer}"),
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
    /// The user reviewed a result and asked for changes. Raised by approval
    /// steps; never needs to be listed in a target's `retry_on`.
    ChangesRequested,
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
    /// Longest an active step may go without any sign of life (agent event,
    /// usage report, checkpoint) before the runner cancels the attempt and
    /// fails it as retryable. Time spent waiting on a permission decision or
    /// while paused does not count. Must exceed the longest legitimate silent
    /// operation, e.g. a tool call (`tool_call_timeout`) or a model's first
    /// event (`first_event_timeout`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_timeout_ms: Option<u64>,
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
