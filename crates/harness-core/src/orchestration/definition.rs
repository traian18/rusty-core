//! Workflow documents and node configurations.
//!
//! Graph bindings, identity, policies, and task queues are grouped in focused
//! child modules and re-exported through the unchanged orchestration facade.

mod bindings;
mod identity;
mod policies;
mod task_queue;

pub use bindings::*;
pub use identity::*;
pub use policies::*;
pub use task_queue::*;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
                context_access: ContextAccess::Own,
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
    /// Approving with notes sends the notes to `revise_target` as changes to
    /// make, and the revised result then passes this step without asking
    /// again. Off, notes given with an approval are only passed on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub revise_on_notes: bool,
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
    /// Which other steps' records (what they did and handed off) this step
    /// is shown, besides the inputs bound to it.
    #[serde(default, skip_serializing_if = "ContextAccess::is_own")]
    pub context_access: ContextAccess,
}

/// Which records of the run's other steps an agent step sees. A step always
/// sees its bound inputs and its own earlier attempts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextAccess {
    /// Only its inputs: a plan or research step judges the request afresh.
    #[default]
    Own,
    /// Every step that ran before it: a builder repairing its work needs to
    /// know what was done and tried.
    All,
    /// The named steps: a verifier sees the work it judges.
    Steps(Vec<OrchestrationNodeId>),
}

impl ContextAccess {
    pub fn is_own(&self) -> bool {
        matches!(self, Self::Own)
    }

    /// Whether the record of `step` is shown.
    pub fn includes(&self, step: &OrchestrationNodeId) -> bool {
        match self {
            Self::Own => false,
            Self::All => true,
            Self::Steps(steps) => steps.contains(step),
        }
    }
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

const fn default_true() -> bool {
    true
}
