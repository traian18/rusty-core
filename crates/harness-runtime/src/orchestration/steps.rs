//! Step executors: the agent-step contract plus the deterministic input,
//! verify, and output nodes. Executors return an outcome; they never choose
//! the next node — that belongs to the reducer.

use std::sync::Arc;

use async_trait::async_trait;
use harness_core::orchestration::{
    AgentContextMode, ApprovalNodeConfig, DelegatedRunRef, Evidence, InputBinding, InputDecision,
    InputNodeConfig, InputRequest, InputResponse, OrchestrationError, OrchestrationNode,
    OrchestrationNodeId, OrchestrationRunId, OrchestrationRunState, OutputBinding, Responder,
    RetryReason, StructuredOutputMode, UsageSummary, VerificationCheck, VerifyNodeConfig,
};
use harness_protocol::{commands::PermissionDecision, events::AgentEventEnvelope};
use serde_json::{json, Map, Value};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::traits::{Workspace, WorkspaceError};

use super::schema::{SchemaResolver, SchemaValidator};

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

impl AgentExecutionError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retry_reason: None,
        }
    }

    pub fn retryable(
        code: impl Into<String>,
        message: impl Into<String>,
        reason: RetryReason,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retry_reason: Some(reason),
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

/// Runs every check (not just until the first failure) so the recorded
/// evidence and the feedback given to a retried producer are complete.
///
/// Returns the verification report as the step output, or a retryable
/// `verification_failed` error. Evidence is returned either way.
pub(crate) async fn execute_verify(
    state: &OrchestrationRunState,
    node: &OrchestrationNode,
    config: &VerifyNodeConfig,
    producers: &(dyn Fn(&OrchestrationNodeId) -> Option<Value> + Sync),
    validation: &Validation<'_>,
    artifacts: &dyn ArtifactResolver,
) -> (Result<Value, OrchestrationError>, Vec<Evidence>) {
    let input = match resolve_inputs(state, &node.input_bindings) {
        Ok(input) => input,
        Err(error) => return (Err(error), Vec::new()),
    };
    let mut evidence = Vec::new();
    let mut manual_checks = Vec::new();
    for check in &config.checks {
        let (passed, detail) = match check {
            VerificationCheck::Criteria {
                pointer,
                plan_pointer,
                block_on,
                defer,
                max_deferred,
            } => {
                let expected = plan_pointer
                    .as_ref()
                    .map(|plan_pointer| plan_criteria(input.pointer(plan_pointer)));
                let judged = judge_criteria(
                    input.pointer(pointer),
                    block_on,
                    defer,
                    *max_deferred,
                    expected.as_deref(),
                );
                manual_checks.extend(judged.deferred);
                (judged.passed, judged.detail)
            }
            VerificationCheck::RequirementsSatisfied {
                plan_pointer,
                results_pointer,
            } => requirement_coverage(input.pointer(plan_pointer), input.pointer(results_pointer)),
            VerificationCheck::Schema => {
                let mut issues = Vec::new();
                for binding in &node.input_bindings {
                    let OutputBinding::NodeOutput { node_id, .. } = &binding.source else {
                        continue;
                    };
                    let Some(schema) = producers(node_id) else {
                        continue;
                    };
                    let value = input.get(&binding.target).unwrap_or(&Value::Null);
                    if let Err(error) = validation.validator.validate(&schema, value) {
                        issues.push(format!("{}: {error}", binding.target));
                    }
                }
                if issues.is_empty() {
                    (true, "bound outputs match their producers' schemas".into())
                } else {
                    (false, issues.join("; "))
                }
            }
            VerificationCheck::RequiredStatus { pointer, equals } => {
                match input.pointer(pointer).and_then(Value::as_str) {
                    Some(actual) if actual == equals => (true, format!("{pointer} == {equals}")),
                    Some(actual) => (
                        false,
                        format!("{pointer} is {actual:?}, expected {equals:?}"),
                    ),
                    None => (false, format!("{pointer} is missing or not a string")),
                }
            }
            VerificationCheck::ArtifactExists { pointer } => {
                let exists = match input.pointer(pointer) {
                    None | Some(Value::Null) => false,
                    Some(Value::String(value)) => !value.is_empty(),
                    Some(Value::Array(value)) => !value.is_empty(),
                    Some(Value::Object(value)) => !value.is_empty(),
                    Some(_) => true,
                };
                if exists {
                    (true, format!("{pointer} is present"))
                } else {
                    (false, format!("{pointer} is missing or empty"))
                }
            }
            VerificationCheck::ArtifactsResolvable { pointer } => {
                match input.pointer(pointer).and_then(Value::as_array) {
                    None => (false, format!("{pointer} is missing or not an array")),
                    Some(items) => {
                        let mut problems = Vec::new();
                        for (index, item) in items.iter().enumerate() {
                            let kind = item.get("kind").and_then(Value::as_str);
                            let reference = item.get("reference").and_then(Value::as_str);
                            match (kind, reference) {
                                (Some(kind), Some(reference)) => {
                                    if let Err(problem) = artifacts.resolve(kind, reference).await {
                                        problems.push(format!("[{index}] {problem}"));
                                    }
                                }
                                _ => problems.push(format!(
                                    "[{index}] artifact needs string kind and reference"
                                )),
                            }
                        }
                        if problems.is_empty() {
                            (true, format!("{} artifact(s) resolved", items.len()))
                        } else {
                            (false, problems.join("; "))
                        }
                    }
                }
            }
        };
        evidence.push(Evidence {
            check: check.id(),
            passed,
            detail,
        });
    }

    let issues: Vec<_> = evidence
        .iter()
        .filter(|item| !item.passed)
        .map(|item| format!("{}: {}", item.check, item.detail))
        .collect();
    let mut report = json!({
        "passed": issues.is_empty(),
        "checks": evidence.iter().map(|item| json!({
            "id": item.check,
            "passed": item.passed,
            "detail": item.detail,
        })).collect::<Vec<_>>(),
        "issues": issues,
    });
    // Always present when criteria are judged, so a later step can bind to it.
    if config
        .checks
        .iter()
        .any(|check| matches!(check, VerificationCheck::Criteria { .. }))
    {
        report["manual_checks"] = Value::Array(manual_checks);
    }
    let result = if issues.is_empty() {
        Ok(report)
    } else if input
        .get("check")
        .and_then(|v| v.get("verdict"))
        .and_then(Value::as_str)
        .is_some_and(|v| v.starts_with("blocked_environment:") || v.starts_with("blocked_user:"))
    {
        Err(OrchestrationError::new(
            "verification_blocked",
            input["check"]["summary"]
                .as_str()
                .unwrap_or("Verification blocked"),
        ))
    } else {
        Err(OrchestrationError::retryable(
            "verification_failed",
            verification_repair_focus(&input, &issues, &config.checks),
            RetryReason::VerificationFailed,
        ))
    };
    (result, evidence)
}

/// The approval step: asks the user to approve `subject`, ask for changes or
/// reject it, unless it is skipped (nothing to review) or the run was
/// started with auto-approve. Requested changes fail the step with
/// `changes_requested`, which the reducer routes to the revise target.
pub(crate) async fn execute_approval(
    state: &OrchestrationRunState,
    node: &OrchestrationNode,
    config: &ApprovalNodeConfig,
    attempt: u32,
    context: &mut StepContext,
) -> Result<Value, OrchestrationError> {
    let subject = match resolve_binding(state, &config.subject) {
        // Nothing where the subject should be is nothing to review.
        Err(error) if error.code == "invalid_pointer" && config.skip_if_empty.is_some() => {
            Value::Null
        }
        result => result?,
    };
    let outcome = |decision: &str, by: Responder, notes: Option<String>| {
        json!({
            "decision": decision,
            "by": by,
            "notes": notes.unwrap_or_default(),
            "subject": subject,
        })
    };
    if let Some(pointer) = &config.skip_if_empty {
        let empty = match subject.pointer(pointer) {
            None | Some(Value::Null) => true,
            Some(Value::String(text)) => text.trim().is_empty(),
            Some(Value::Array(items)) => items.is_empty(),
            Some(Value::Object(fields)) => fields.is_empty(),
            Some(_) => false,
        };
        if empty {
            return Ok(outcome("skipped", Responder::Auto, None));
        }
    }
    if state.options.auto_approve && config.allow_auto_approve {
        return Ok(outcome("approved", Responder::Auto, None));
    }
    let decision = |id: &str, label: &str, requires_text: bool| InputDecision {
        id: id.into(),
        label: label.into(),
        requires_text,
    };
    let mut decisions = vec![decision("approve", "Approve", false)];
    if config.revise_target().is_some() && attempt <= config.max_revisions {
        decisions.push(decision("request_changes", "Request changes", true));
    }
    decisions.push(decision("reject", "Reject", false));
    let prompt = if config.prompt.trim().is_empty() {
        format!("Review {} before the workflow continues.", node.name)
    } else {
        config.prompt.clone()
    };
    let response = context
        .ask(InputRequest {
            id: format!("{}:{attempt}", node.id),
            kind: "approval".into(),
            prompt,
            subject: subject.clone(),
            decisions,
        })
        .await?;
    let notes = response.text.clone().filter(|text| !text.trim().is_empty());
    match response.decision.as_str() {
        "approve" => Ok(outcome("approved", response.by, notes)),
        "request_changes" => Err(OrchestrationError::retryable(
            CHANGES_REQUESTED,
            notes.unwrap_or_default(),
            RetryReason::ChangesRequested,
        )),
        _ => Err(OrchestrationError::new(
            "approval_rejected",
            match notes {
                Some(notes) => format!("{} was rejected: {notes}", node.name),
                None => format!("{} was rejected.", node.name),
            },
        )),
    }
}

/// Feedback code for changes a user asked for at an approval step. Its
/// message is the user's own request, not model output.
pub const CHANGES_REQUESTED: &str = "changes_requested";

struct JudgedCriteria {
    passed: bool,
    detail: String,
    /// `{id, how_to_test, evidence}` for every deferred criterion.
    deferred: Vec<Value>,
}

fn criterion_status(criterion: &Value) -> String {
    criterion["status"]
        .as_str()
        .unwrap_or("unverified")
        .trim()
        .to_lowercase()
}

fn criterion_id(criterion: &Value, index: usize) -> String {
    criterion["id"]
        .as_str()
        .filter(|id| !id.trim().is_empty())
        .map_or_else(|| format!("criterion {}", index + 1), str::to_owned)
}

/// The acceptance criterion ids of a task plan.
fn plan_criteria(plan: Option<&Value>) -> Vec<String> {
    plan.and_then(|plan| plan["requirements"].as_array())
        .into_iter()
        .flatten()
        .flat_map(|requirement| requirement["criteria"].as_array().into_iter().flatten())
        .filter_map(|criterion| criterion["id"].as_str().map(str::to_owned))
        .collect()
}

/// Judges `value`, the reported criteria. With `expected` (a plan's criterion
/// ids) every one must also have been reported; ids match loosely.
fn judge_criteria(
    value: Option<&Value>,
    block_on: &[String],
    defer: &[String],
    max_deferred: Option<u32>,
    expected: Option<&[String]>,
) -> JudgedCriteria {
    let failed = |detail: String| JudgedCriteria {
        passed: false,
        detail,
        deferred: Vec::new(),
    };
    let Some(criteria) = value.and_then(Value::as_array) else {
        return failed("criteria are missing or not an array".into());
    };
    if criteria.is_empty() {
        return failed("no criteria were reported".into());
    }
    let mut blocking = Vec::new();
    let mut deferred = Vec::new();
    for (index, criterion) in criteria.iter().enumerate() {
        let status = criterion_status(criterion);
        let id = criterion_id(criterion, index);
        if status == "pass" {
            continue;
        }
        // Anything not explicitly deferred blocks, including statuses the
        // check does not name: an unknown status is not a pass.
        if !block_on.contains(&status) && defer.contains(&status) {
            let text = |field: &str| {
                criterion[field]
                    .as_str()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
            };
            deferred.push(json!({
                "id": id,
                "how_to_test": text("how_to_test").or(text("evidence")).unwrap_or("Check this by hand."),
                "evidence": text("evidence").unwrap_or(""),
            }));
        } else {
            blocking.push(format!("{id} [{status}]"));
        }
    }
    if let Some(expected) = expected {
        let wanted: Vec<&str> = expected.iter().map(String::as_str).collect();
        let found = match_results(&wanted, criteria, |c| c["id"].as_str());
        for (id, result) in wanted.iter().zip(found) {
            if result.is_none() {
                blocking.push(format!("{id} [missing]"));
            }
        }
    }
    if let Some(max) = max_deferred.filter(|max| deferred.len() > *max as usize) {
        blocking.push(format!(
            "{} criteria were left for manual checks, at most {max} may be",
            deferred.len()
        ));
    }
    if !blocking.is_empty() {
        return failed(format!("not met: {}", blocking.join(", ")));
    }
    let passed = criteria.len() - deferred.len();
    let detail = if deferred.is_empty() {
        format!("{passed} criteria passed")
    } else {
        format!(
            "{passed} criteria passed, {} left for manual checks",
            deferred.len()
        )
    };
    JudgedCriteria {
        passed: true,
        detail,
        deferred,
    }
}

/// What a producer sent back by a failed verification should fix. Criteria a
/// `criteria` check defers (only a person can check them) are listed apart so
/// the producer does not try to "fix" something that is not broken.
fn verification_repair_focus(
    input: &Value,
    issues: &[String],
    checks: &[VerificationCheck],
) -> String {
    let mut parts = vec![format!("Verification did not pass: {}", issues.join("; "))];
    let line = |index: usize, criterion: &Value| {
        let evidence = criterion["evidence"]
            .as_str()
            .unwrap_or("No evidence supplied");
        format!(
            "- {} [{}]: {evidence}",
            criterion_id(criterion, index),
            criterion_status(criterion)
        )
    };
    let mut unresolved = Vec::new();
    let mut manual = Vec::new();
    let criteria_checks: Vec<_> = checks
        .iter()
        .filter_map(|check| match check {
            VerificationCheck::Criteria {
                pointer,
                plan_pointer,
                block_on,
                defer,
                ..
            } => Some((pointer, plan_pointer, block_on, defer)),
            _ => None,
        })
        .collect();
    if criteria_checks.is_empty() {
        let criteria = input["check"]["criteria"].as_array().into_iter().flatten();
        for (index, criterion) in criteria.enumerate() {
            if criterion_status(criterion) != "pass" {
                unresolved.push(line(index, criterion));
            }
        }
    }
    for (pointer, plan_pointer, block_on, defer) in criteria_checks {
        let criteria = input.pointer(pointer).and_then(Value::as_array);
        // Plan criteria nobody reported are unmet too.
        if let Some(plan_pointer) = plan_pointer {
            let expected = plan_criteria(input.pointer(plan_pointer));
            let wanted: Vec<&str> = expected.iter().map(String::as_str).collect();
            let found = match_results(&wanted, criteria.map_or(&[][..], Vec::as_slice), |c| {
                c["id"].as_str()
            });
            for (id, result) in wanted.iter().zip(found) {
                if result.is_none() {
                    unresolved.push(format!("- {id} [missing]: the review did not report it"));
                }
            }
        }
        for (index, criterion) in criteria.into_iter().flatten().enumerate() {
            let status = criterion_status(criterion);
            if status == "pass" {
                continue;
            }
            if !block_on.contains(&status) && defer.contains(&status) {
                manual.push(line(index, criterion));
            } else {
                unresolved.push(line(index, criterion));
            }
        }
    }
    if !unresolved.is_empty() {
        parts.push(format!(
            "Focus on these unmet criteria:\n{}",
            unresolved.join("\n")
        ));
    }
    if !manual.is_empty() {
        parts.push(format!(
            "Left for the user to check by hand; do not try to fix these:\n{}",
            manual.join("\n")
        ));
    }
    if let Some(summary) = input["check"]["summary"].as_str() {
        parts.push(format!("Review summary: {summary}"));
    }
    parts.join("\n\n")
}

/// Which result answers which expected criterion. IDs are matched loosely
/// (`C-1`, `c1` and `C1` are one); when the results carry none of the expected
/// IDs (a reply that numbered them its own way) the same number of results
/// answers the criteria in order.
pub(crate) fn match_results<'a>(
    expected: &[&str],
    results: &'a [Value],
    id_of: impl Fn(&Value) -> Option<&str>,
) -> Vec<Option<&'a Value>> {
    let key = |id: &str| {
        id.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let by_id: Vec<Option<&Value>> = expected
        .iter()
        .map(|id| {
            results
                .iter()
                .find(|result| id_of(result).is_some_and(|found| key(found) == key(id)))
        })
        .collect();
    if by_id.iter().all(Option::is_none) && results.len() == expected.len() {
        return results.iter().map(Some).collect();
    }
    by_id
}

/// A missing or failed criterion must never be mistaken for acceptance.
pub(crate) fn requirement_coverage(
    plan: Option<&Value>,
    results: Option<&Value>,
) -> (bool, String) {
    let expected: Vec<_> = plan
        .and_then(|p| p["requirements"].as_array())
        .into_iter()
        .flatten()
        .flat_map(|r| r["criteria"].as_array().into_iter().flatten())
        .filter_map(|c| c["id"].as_str())
        .collect();
    let Some(results) = results.and_then(Value::as_array) else {
        return (false, "criterion results are missing".into());
    };
    let matched = match_results(&expected, results, |r| r["id"].as_str());
    let passed = !expected.is_empty()
        && matched.iter().all(|result| {
            result.is_some_and(|r| {
                r["status"] == "pass"
                    && r["evidence"].as_str().is_some_and(|s| !s.trim().is_empty())
            })
        });
    (
        passed,
        if passed {
            "all acceptance criteria have passing evidence"
        } else {
            "missing, failed or unevidenced acceptance criteria"
        }
        .into(),
    )
}

#[cfg(test)]
mod verification_feedback_tests {
    use super::*;

    #[test]
    fn repair_feedback_names_only_unmet_criteria_and_their_evidence() {
        let input = json!({"check": {
            "summary": "Native notifications still need work.",
            "criteria": [
                {"id": "C1", "status": "pass", "evidence": "Types exist."},
                {"id": "C2", "status": "fail", "evidence": "No native delivery in notifications.rs."},
                {"id": "C3", "status": "unverified", "evidence": "No question adapter test."}
            ]
        }});
        let focus = verification_repair_focus(&input, &["verdict was fail".into()], &[]);
        assert!(focus.contains("C2 [fail]: No native delivery in notifications.rs."));
        assert!(focus.contains("C3 [unverified]: No question adapter test."));
        assert!(!focus.contains("C1 [pass]"));
        assert!(focus.contains("Review summary: Native notifications still need work."));
    }

    fn criteria_check(max_deferred: Option<u32>) -> VerificationCheck {
        VerificationCheck::Criteria {
            pointer: "/check/criteria".into(),
            plan_pointer: None,
            block_on: vec!["fail".into()],
            defer: vec!["manual".into()],
            max_deferred,
        }
    }

    fn judge(criteria: Value, max_deferred: Option<u32>) -> JudgedCriteria {
        let VerificationCheck::Criteria {
            block_on, defer, ..
        } = criteria_check(max_deferred)
        else {
            unreachable!()
        };
        judge_criteria(Some(&criteria), &block_on, &defer, max_deferred, None)
    }

    #[test]
    fn manual_criteria_let_the_work_move_on_and_become_manual_checks() {
        let judged = judge(
            json!([
                {"id": "C1", "status": "pass", "evidence": "Unit test passes."},
                {"id": "C2", "status": "manual", "evidence": "Needs a phone.", "how_to_test": "Open the app on iOS and tap Share."}
            ]),
            None,
        );
        assert!(judged.passed, "{}", judged.detail);
        assert_eq!(judged.detail, "1 criteria passed, 1 left for manual checks");
        assert_eq!(
            judged.deferred,
            vec![
                json!({"id": "C2", "how_to_test": "Open the app on iOS and tap Share.", "evidence": "Needs a phone."})
            ]
        );
    }

    #[test]
    fn failed_unknown_or_missing_criteria_block() {
        let failed = judge(
            json!([{"id": "C1", "status": "fail", "evidence": "x"}]),
            None,
        );
        assert!(!failed.passed);
        assert_eq!(failed.detail, "not met: C1 [fail]");
        let unknown = judge(
            json!([{"id": "C1", "status": "unverified", "evidence": "x"}]),
            None,
        );
        assert!(!unknown.passed, "an undeclared status is not a pass");
        assert!(!judge(json!([]), None).passed, "no criteria is not a pass");
        assert!(!judge_criteria(None, &["fail".into()], &[], None, None).passed);
    }

    #[test]
    fn plan_criteria_nobody_reported_block_as_missing() {
        let plan = json!({"requirements": [{"id": "R1", "text": "t", "criteria": [{"id": "C-1", "text": "a"}, {"id": "C2", "text": "b"}]}]});
        let expected = plan_criteria(Some(&plan));
        assert_eq!(expected, ["C-1", "C2"]);
        let reported = json!([{"id": "c1", "status": "pass", "evidence": "x"}]);
        let block = ["fail".to_string()];
        let judged = judge_criteria(Some(&reported), &block, &[], None, Some(&expected));
        assert!(!judged.passed);
        assert_eq!(judged.detail, "not met: C2 [missing]");
        let all = json!([{"id": "C1", "status": "pass"}, {"id": "C2", "status": "pass"}]);
        assert!(judge_criteria(Some(&all), &block, &[], None, Some(&expected)).passed);
        let input = json!({"check": {"criteria": reported}, "plan": plan});
        let check = VerificationCheck::Criteria {
            pointer: "/check/criteria".into(),
            plan_pointer: Some("/plan".into()),
            block_on: block.to_vec(),
            defer: vec![],
            max_deferred: None,
        };
        let focus = verification_repair_focus(&input, &["criteria".into()], &[check]);
        assert!(focus.contains("C2 [missing]"), "{focus}");
    }

    #[test]
    fn deferring_more_than_allowed_blocks() {
        let criteria = json!([
            {"id": "C1", "status": "manual", "evidence": "a"},
            {"id": "C2", "status": "manual", "evidence": "b"}
        ]);
        assert!(judge(criteria.clone(), Some(2)).passed);
        let judged = judge(criteria, Some(1));
        assert!(!judged.passed);
        assert!(judged.detail.contains("at most 1"), "{}", judged.detail);
    }

    #[test]
    fn repair_feedback_keeps_manual_criteria_apart_from_unmet_ones() {
        let input = json!({"check": {"criteria": [
            {"id": "C1", "status": "fail", "evidence": "Button does nothing."},
            {"id": "C2", "status": "manual", "evidence": "Needs a printer."}
        ]}});
        let focus =
            verification_repair_focus(&input, &["criteria".into()], &[criteria_check(None)]);
        let (unmet, manual) = focus
            .split_once("Left for the user to check by hand")
            .expect("manual section");
        assert!(unmet.contains("C1 [fail]: Button does nothing."));
        assert!(!unmet.contains("C2"));
        assert!(manual.contains("C2 [manual]: Needs a printer."));
    }
}
