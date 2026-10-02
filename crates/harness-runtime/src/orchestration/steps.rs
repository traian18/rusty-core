//! Step executors: the agent-step contract plus the deterministic input,
//! verify, and output nodes. Executors return an outcome; they never choose
//! the next node — that belongs to the reducer.

use std::sync::Arc;

use async_trait::async_trait;
use harness_core::orchestration::{
    AgentContextMode, DelegatedRunRef, Evidence, InputBinding, InputNodeConfig, OrchestrationError,
    OrchestrationNode, OrchestrationNodeId, OrchestrationRunId, OrchestrationRunState,
    OutputBinding, RetryReason, StructuredOutputMode, UsageSummary, VerificationCheck,
    VerifyNodeConfig,
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
}

#[derive(Debug, Clone)]
pub struct PermissionResolution {
    pub permission_id: String,
    pub decision: PermissionDecision,
}

/// Per-attempt channel between the runner and an executor.
pub struct StepContext {
    pub cancellation: CancellationToken,
    signals: mpsc::UnboundedSender<StepSignal>,
    /// Decisions for permissions this attempt reported.
    pub permissions: mpsc::UnboundedReceiver<PermissionResolution>,
}

impl StepContext {
    pub(crate) fn new(
        cancellation: CancellationToken,
        signals: mpsc::UnboundedSender<StepSignal>,
        permissions: mpsc::UnboundedReceiver<PermissionResolution>,
    ) -> Self {
        Self {
            cancellation,
            signals,
            permissions,
        }
    }

    /// A context whose signals go nowhere and that never receives decisions.
    pub fn detached(cancellation: CancellationToken) -> Self {
        let (signals, _) = mpsc::unbounded_channel();
        let (_, permissions) = mpsc::unbounded_channel();
        Self::new(cancellation, signals, permissions)
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
    for check in &config.checks {
        let (passed, detail) = match check {
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
    let report = json!({
        "passed": issues.is_empty(),
        "checks": evidence.iter().map(|item| json!({
            "id": item.check,
            "passed": item.passed,
            "detail": item.detail,
        })).collect::<Vec<_>>(),
        "issues": issues,
    });
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
            verification_repair_focus(&input, &issues),
            RetryReason::VerificationFailed,
        ))
    };
    (result, evidence)
}

fn verification_repair_focus(input: &Value, issues: &[String]) -> String {
    let mut parts = vec![format!("Verification did not pass: {}", issues.join("; "))];
    let unresolved = input["check"]["criteria"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|criterion| criterion["status"].as_str() != Some("pass"))
        .map(|criterion| {
            let id = criterion["id"].as_str().unwrap_or("unnamed criterion");
            let status = criterion["status"].as_str().unwrap_or("unverified");
            let evidence = criterion["evidence"]
                .as_str()
                .unwrap_or("No evidence supplied");
            format!("- {id} [{status}]: {evidence}")
        })
        .collect::<Vec<_>>();
    if !unresolved.is_empty() {
        parts.push(format!(
            "Focus on these unmet criteria:\n{}",
            unresolved.join("\n")
        ));
    }
    if let Some(summary) = input["check"]["summary"].as_str() {
        parts.push(format!("Review summary: {summary}"));
    }
    parts.join("\n\n")
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
        let focus = verification_repair_focus(&input, &["verdict was fail".into()]);
        assert!(focus.contains("C2 [fail]: No native delivery in notifications.rs."));
        assert!(focus.contains("C3 [unverified]: No question adapter test."));
        assert!(!focus.contains("C1 [pass]"));
        assert!(focus.contains("Review summary: Native notifications still need work."));
    }
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
