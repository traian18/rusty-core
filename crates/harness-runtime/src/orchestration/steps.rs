//! Step executors: the agent-step contract plus the deterministic input,
//! verify, and output nodes. Executors return an outcome; they never choose
//! the next node — that belongs to the reducer.
//!
//! The verify node lives in [`verify`], the criteria it judges in
//! [`criteria`], and the approval node in [`approval`].

mod approval;
mod criteria;
mod verify;

use std::sync::Arc;

use async_trait::async_trait;
use harness_core::orchestration::{
    AgentContextMode, DelegatedRunRef, InputBinding, InputNodeConfig, InputRequest, InputResponse,
    OrchestrationError, OrchestrationNodeId, OrchestrationRunId, OrchestrationRunState,
    OutputBinding, RetryReason, StructuredOutputMode, UsageSummary,
};
use harness_protocol::{commands::PermissionDecision, events::AgentEventEnvelope};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::traits::{Workspace, WorkspaceError};

use super::schema::{SchemaResolver, SchemaValidator};

pub(crate) use approval::execute_approval;
pub use approval::{is_user_change, CHANGES_APPROVED, CHANGES_REQUESTED};
pub(crate) use criteria::match_results;
#[cfg(test)]
pub(crate) use criteria::requirement_coverage;
pub(crate) use verify::execute_verify;

// ---------------------------------------------------------------------------
// Agent-step contract
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct AgentStepRequest {
    pub task_queue: Option<harness_core::orchestration::TaskQueueConfig>,
    pub checkpoint: Option<Value>,
    pub run_id: OrchestrationRunId,
    pub node_id: OrchestrationNodeId,
    pub attempt: u32,
    pub instructions: String,
    /// Bound step input. Untrusted data: it may contain upstream model or
    /// tool output and must be presented to the model as data.
    pub input: Value,
    pub output_schema: Value,
    /// Resolved, explicit allow list. Enforced at description and execution.
    pub tools: Vec<String>,
    pub model: Option<String>,
    pub structured_output: StructuredOutputMode,
    pub context_mode: AgentContextMode,
    /// Behavior profile for the delegated agent; `None` inherits the
    /// parent session's active profile.
    pub profile: Option<harness_core::behavior::ProfileRef>,
    /// Why earlier attempts were rejected (validation or verification
    /// errors). Untrusted data, like `input`.
    pub feedback: Vec<OrchestrationError>,
    /// A task queue may offer to revise the plan: failing with
    /// `changes_requested` re-runs the step that wrote it.
    pub plan_revisable: bool,
    /// How deep a flow this step starts would be, and the run's options: for
    /// the flows a task queue runs.
    pub subflow_depth: u32,
    pub run_options: harness_core::orchestration::RunOptions,
    /// What this step's previous attempt did and handed on, when it runs
    /// again. Untrusted data, like `input`.
    pub previous_attempt: Option<String>,
    /// The records of the other steps its `context_access` lets it see.
    /// Untrusted data, like `input`.
    pub journal: Option<String>,
    /// Whether, and how, the step may ask for research.
    pub research: harness_core::orchestration::ResearchPolicy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentStepOutput {
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct AgentExecutionError {
    pub code: String,
    pub message: String,
    pub retry_reason: Option<RetryReason>,
}

/// Built through [`OrchestrationError`]'s constructors, which this error
/// mirrors field for field.
impl AgentExecutionError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        OrchestrationError::new(code, message).into()
    }

    pub fn retryable(
        code: impl Into<String>,
        message: impl Into<String>,
        reason: RetryReason,
    ) -> Self {
        OrchestrationError::retryable(code, message, reason).into()
    }
}

impl From<OrchestrationError> for AgentExecutionError {
    fn from(error: OrchestrationError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            retry_reason: error.retry_reason,
        }
    }
}

impl From<AgentExecutionError> for OrchestrationError {
    fn from(error: AgentExecutionError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            retry_reason: error.retry_reason,
        }
    }
}

/// Observations an executor reports while an attempt is in flight.
#[derive(Debug)]
pub enum StepSignal {
    Checkpoint {
        value: Value,
        committed: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// The session/agent/run executing the attempt. Sent as soon as known so
    /// even failed or cancelled attempts are correlated.
    Delegated(DelegatedRunRef),
    /// The delegated agent is blocked on a tool permission decision.
    PermissionRequested {
        permission_id: String,
        tool_name: String,
    },
    /// Cumulative usage of the attempt so far (replaces earlier reports).
    Usage(UsageSummary),
    /// A raw event from the delegated agent, republished with correlation.
    Agent(Box<AgentEventEnvelope>),
    /// The attempt asks the user a question and waits for the answer (see
    /// [`StepContext::ask`]).
    InputRequested(InputRequest),
}

#[derive(Debug, Clone)]
pub struct PermissionResolution {
    pub permission_id: String,
    pub decision: PermissionDecision,
}

/// The user's answer to a question an attempt asked.
#[derive(Debug, Clone)]
pub struct InputAnswer {
    pub request_id: String,
    pub response: InputResponse,
}

/// Per-attempt channel between the runner and an executor.
pub struct StepContext {
    pub cancellation: CancellationToken,
    signals: mpsc::UnboundedSender<StepSignal>,
    /// Decisions for permissions this attempt reported.
    pub permissions: mpsc::UnboundedReceiver<PermissionResolution>,
    answers: mpsc::UnboundedReceiver<InputAnswer>,
}

impl StepContext {
    pub(crate) fn new(
        cancellation: CancellationToken,
        signals: mpsc::UnboundedSender<StepSignal>,
        permissions: mpsc::UnboundedReceiver<PermissionResolution>,
        answers: mpsc::UnboundedReceiver<InputAnswer>,
    ) -> Self {
        Self {
            cancellation,
            signals,
            permissions,
            answers,
        }
    }

    /// A context whose signals go nowhere and that never receives decisions.
    pub fn detached(cancellation: CancellationToken) -> Self {
        let (signals, _) = mpsc::unbounded_channel();
        let (_, permissions) = mpsc::unbounded_channel();
        let (_, answers) = mpsc::unbounded_channel();
        Self::new(cancellation, signals, permissions, answers)
    }

    /// Ask the user `request` and wait for the answer. The run shows as
    /// waiting for input meanwhile, and the wait does not count as a stall.
    pub async fn ask(
        &mut self,
        request: InputRequest,
    ) -> Result<InputResponse, AgentExecutionError> {
        let id = request.id.clone();
        self.signals
            .send(StepSignal::InputRequested(request))
            .map_err(|_| AgentExecutionError::new("cancelled", "workflow stopped before asking"))?;
        loop {
            tokio::select! {
                _ = self.cancellation.cancelled() => {
                    return Err(AgentExecutionError::new("cancelled", "workflow cancelled while waiting for an answer"));
                }
                answer = self.answers.recv() => match answer {
                    Some(answer) if answer.request_id == id => return Ok(answer.response),
                    Some(_) => continue,
                    None => return Err(AgentExecutionError::new("cancelled", "workflow stopped while waiting for an answer")),
                },
            }
        }
    }

    pub async fn checkpoint(&self, value: Value) -> Result<(), AgentExecutionError> {
        let (committed, ack) = tokio::sync::oneshot::channel();
        self.signals
            .send(StepSignal::Checkpoint { value, committed })
            .map_err(|_| {
                AgentExecutionError::new("checkpoint_failed", "workflow stopped before checkpoint")
            })?;
        ack.await
            .map_err(|_| {
                AgentExecutionError::new("checkpoint_failed", "checkpoint was not committed")
            })?
            .map_err(|error| AgentExecutionError::new("checkpoint_failed", error))
    }

    pub fn signal(&self, signal: StepSignal) {
        // The runner may have stopped listening (e.g. cancellation); signals
        // are observations, so dropping them then is harmless.
        let _ = self.signals.send(signal);
    }
}

/// A child run a `subflow` node asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct SubflowRequest {
    pub run_id: OrchestrationRunId,
    pub node_id: OrchestrationNodeId,
    pub attempt: u32,
    pub target: harness_core::orchestration::SubflowTarget,
    /// The values bound to the node: the child's run input.
    pub input: Value,
    /// How deep the child is: a flow run on its own is level 0.
    pub depth: u32,
    pub options: harness_core::orchestration::RunOptions,
}

/// Runs the child of a `subflow` node to its end and returns its output. The
/// engine supplies it, since it knows the other flows and the session the
/// child shares tools and permissions with.
#[async_trait]
pub trait SubflowExecutor: Send + Sync {
    async fn execute(
        &self,
        request: SubflowRequest,
        context: &mut StepContext,
    ) -> Result<Value, AgentExecutionError>;
}

#[async_trait]
pub trait AgentStepExecutor: Send + Sync {
    async fn execute(
        &self,
        request: AgentStepRequest,
        context: StepContext,
    ) -> Result<AgentStepOutput, AgentExecutionError>;
}

// ---------------------------------------------------------------------------
// Artifact resolution
// ---------------------------------------------------------------------------

/// Decides whether a `{kind, reference}` artifact claimed by a model exists.
#[async_trait]
pub trait ArtifactResolver: Send + Sync {
    async fn resolve(&self, kind: &str, reference: &str) -> Result<(), String>;
}

/// Accepts any non-empty reference. Performs no I/O, so it cannot catch a
/// model claiming a file it never wrote; use [`WorkspaceArtifactResolver`]
/// when a workspace is available.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReferenceArtifactResolver;

#[async_trait]
impl ArtifactResolver for ReferenceArtifactResolver {
    async fn resolve(&self, _kind: &str, reference: &str) -> Result<(), String> {
        if reference.trim().is_empty() {
            Err("reference is empty".into())
        } else {
            Ok(())
        }
    }
}

/// Resolves `file`/`path` artifacts against the workspace; other kinds are
/// opaque references that only need to be non-empty.
pub struct WorkspaceArtifactResolver {
    workspace: Arc<dyn Workspace>,
}

impl WorkspaceArtifactResolver {
    pub fn new(workspace: Arc<dyn Workspace>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl ArtifactResolver for WorkspaceArtifactResolver {
    async fn resolve(&self, kind: &str, reference: &str) -> Result<(), String> {
        ReferenceArtifactResolver.resolve(kind, reference).await?;
        if !matches!(kind, "file" | "path") {
            return Ok(());
        }
        match self.workspace.read(reference).await {
            Ok(_) => Ok(()),
            // Present but not UTF-8 text: it exists.
            Err(WorkspaceError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData => {
                Ok(())
            }
            Err(WorkspaceError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(format!("{reference} does not exist in the workspace"))
            }
            Err(error) => Err(format!("{reference} could not be resolved: {error}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Bindings
// ---------------------------------------------------------------------------

pub(crate) fn resolve_inputs(
    state: &OrchestrationRunState,
    bindings: &[InputBinding],
) -> Result<Value, OrchestrationError> {
    let mut object = Map::new();
    for binding in bindings {
        object.insert(
            binding.target.clone(),
            resolve_binding(state, &binding.source)?,
        );
    }
    Ok(Value::Object(object))
}

pub(crate) fn resolve_binding(
    state: &OrchestrationRunState,
    binding: &OutputBinding,
) -> Result<Value, OrchestrationError> {
    let (value, pointer) = match binding {
        OutputBinding::RunInput { pointer } => (
            state.input.as_ref().ok_or_else(|| {
                OrchestrationError::new("missing_input", "run input is unavailable")
            })?,
            pointer,
        ),
        OutputBinding::NodeOutput { node_id, pointer } => (
            state
                .steps
                .get(node_id)
                .and_then(|step| step.output.as_ref())
                .ok_or_else(|| {
                    OrchestrationError::new(
                        "missing_output",
                        format!("node {node_id} has no output"),
                    )
                })?,
            pointer,
        ),
    };
    pointer_value(value, pointer).cloned()
}

fn pointer_value<'a>(value: &'a Value, pointer: &str) -> Result<&'a Value, OrchestrationError> {
    value.pointer(pointer).ok_or_else(|| {
        OrchestrationError::new(
            "invalid_pointer",
            format!("JSON pointer {pointer} did not resolve"),
        )
    })
}

// ---------------------------------------------------------------------------
// Deterministic nodes
// ---------------------------------------------------------------------------

pub(crate) struct Validation<'a> {
    pub schemas: &'a dyn SchemaResolver,
    pub validator: &'a dyn SchemaValidator,
}

impl Validation<'_> {
    pub fn resolve(
        &self,
        reference: &harness_core::orchestration::SchemaReference,
    ) -> Result<Value, OrchestrationError> {
        self.schemas
            .resolve(reference)
            .map_err(|error| OrchestrationError::new("schema_resolution_failed", error.message))
    }

    pub fn check(
        &self,
        schema: &Value,
        value: &Value,
        code: &str,
        retry_reason: Option<RetryReason>,
    ) -> Result<(), OrchestrationError> {
        self.validator.validate(schema, value).map_err(|error| {
            let message = error.to_string();
            match retry_reason {
                Some(reason) => OrchestrationError::retryable(code, message, reason),
                None => OrchestrationError::new(code, message),
            }
        })
    }
}

pub(crate) fn execute_input(
    state: &OrchestrationRunState,
    config: &InputNodeConfig,
    input_schema: Option<&harness_core::orchestration::SchemaReference>,
    validation: &Validation<'_>,
) -> Result<Value, OrchestrationError> {
    let mut input = state.input.clone().unwrap_or(Value::Null);
    if !config.defaults.is_empty() {
        let object = input.as_object_mut().ok_or_else(|| {
            OrchestrationError::new("invalid_input", "input defaults require an object input")
        })?;
        for (key, value) in &config.defaults {
            object.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    if let Some(reference) = input_schema {
        let schema = validation.resolve(reference)?;
        validation.check(&schema, &input, "invalid_input", None)?;
    }
    Ok(input)
}
