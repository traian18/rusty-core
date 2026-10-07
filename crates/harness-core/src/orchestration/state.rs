use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::compiler::CompiledOrchestration;
use super::definition::{
    EdgeCondition, OrchestrationDefinitionId, OrchestrationNodeId, OrchestrationNodeKind,
    OrchestrationRunId, RetryReason,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationStatus {
    /// Run identity exists, no input admitted.
    Created,
    /// A step may be scheduled.
    Ready,
    /// A step is executing.
    Running,
    /// The running step's delegated agent awaits a tool permission decision.
    WaitingForPermission,
    /// The running step asked the user a question (an approval, or what to
    /// do after a failure) and waits for the answer.
    WaitingForInput,
    /// No new work is admitted. A step already running may still finish.
    Paused,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

impl OrchestrationStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Ready,
    Running,
    WaitingForPermission,
    WaitingForInput,
    RetryScheduled,
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
}

impl StepStatus {
    /// A step with an attempt currently in flight.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Running | Self::WaitingForPermission | Self::WaitingForInput
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationRunState {
    pub run_id: OrchestrationRunId,
    pub definition_id: OrchestrationDefinitionId,
    pub definition_revision: u64,
    /// Content hash of the definition this run was started against.
    pub definition_hash: String,
    pub status: OrchestrationStatus,
    /// Set by `Pause`, cleared by `Resume`. Kept separately from `status`
    /// because a pause requested mid-step takes effect when the step settles.
    #[serde(default)]
    pub paused: bool,
    pub input: Option<Value>,
    pub steps: BTreeMap<OrchestrationNodeId, StepRun>,
    pub final_output: Option<Value>,
    pub error: Option<OrchestrationError>,
    /// The terminal failure boundary; completed upstream steps remain reusable.
    #[serde(default)]
    pub failed_step: Option<OrchestrationNodeId>,
    pub total_attempts: u32,
    #[serde(default)]
    pub usage: UsageSummary,
    /// How the run was started; kept with the state so a resumed run behaves the same.
    #[serde(default)]
    pub options: RunOptions,
}

/// Choices made when a run is started rather than in its definition.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOptions {
    /// Approval steps that allow it pass without asking, recorded as
    /// answered by `auto`. Questions about failures are still asked.
    #[serde(default)]
    pub auto_approve: bool,
}

impl OrchestrationRunState {
    pub fn new(run_id: OrchestrationRunId, compiled: &CompiledOrchestration) -> Self {
        let steps = compiled
            .nodes
            .keys()
            .cloned()
            .map(|id| (id, StepRun::default()))
            .collect();
        Self {
            run_id,
            definition_id: compiled.definition_id.clone(),
            definition_revision: compiled.revision,
            definition_hash: compiled.content_hash.clone(),
            status: OrchestrationStatus::Created,
            paused: false,
            input: None,
            steps,
            final_output: None,
            error: None,
            failed_step: None,
            total_attempts: 0,
            usage: UsageSummary::default(),
            options: RunOptions::default(),
        }
    }

    /// The step whose attempt is currently in flight, if any.
    pub fn active_step(&self) -> Option<(&OrchestrationNodeId, &StepRun)> {
        self.steps.iter().find(|(_, step)| step.status.is_active())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRun {
    /// Durable task progress. Kept on a retry of this step, cleared when an upstream step is invalidated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<Value>,
    pub status: StepStatus,
    /// Every attempt, in order. Never overwritten on retry.
    pub attempts: Vec<StepAttempt>,
    pub output: Option<Value>,
    pub error: Option<OrchestrationError>,
    /// Errors that caused this step to be re-run — its own failures and
    /// downstream verification failures — provided to the next attempt as data.
    #[serde(default)]
    pub feedback: Vec<OrchestrationError>,
    /// Permissions the active attempt is blocked on (parallel tool calls
    /// can request several at once).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_permissions: Vec<String>,
    /// The question the active attempt is waiting on the user to answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_input: Option<InputRequest>,
    /// Attempts made before the user last asked for changes to this step.
    /// Its own retry budget, and a verifier's, count from here, so asking
    /// for changes never uses up the retries a failure would get.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retry_base: u32,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// A question a step puts to the user; the run waits until it is answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputRequest {
    /// Unique within the run.
    pub id: String,
    /// What is being asked, e.g. `approval` or `task_failure`, so a host can
    /// present it suitably.
    pub kind: String,
    pub prompt: String,
    /// The data the user decides on (a plan, a failure report, ...).
    #[serde(default)]
    pub subject: Value,
    /// The answers the user may give, in display order.
    pub decisions: Vec<InputDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputDecision {
    pub id: String,
    pub label: String,
    /// The answer must carry text (e.g. the changes being asked for).
    #[serde(default)]
    pub requires_text: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputResponse {
    /// One of the request's decision ids.
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub by: Responder,
}

/// Who answered a question.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Responder {
    #[default]
    User,
    /// The run was started with auto-approve.
    Auto,
}

impl Default for StepRun {
    fn default() -> Self {
        Self {
            checkpoint: None,
            status: StepStatus::Pending,
            attempts: Vec::new(),
            output: None,
            error: None,
            feedback: Vec::new(),
            pending_permissions: Vec::new(),
            pending_input: None,
            retry_base: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepAttempt {
    pub attempt: u32,
    pub status: AttemptStatus,
    pub output: Option<Value>,
    pub error: Option<OrchestrationError>,
    #[serde(default)]
    pub details: AttemptDetails,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// Observations the runtime reports about a finished attempt. The reducer
/// records them verbatim; timestamps come from the runtime because the core
/// performs no I/O (including reading the clock).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AttemptDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
    /// The exact bound input the step executed with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated: Option<DelegatedRunRef>,
    /// The resolved tool allow list for an agent attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub usage: UsageSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

/// Correlation to the session/agent/run that executed a delegated attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegatedRunRef {
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_run_id: Option<String>,
}

/// A recorded claim about why a criterion is (or is not) satisfied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub check: String,
    pub passed: bool,
    pub detail: String,
}

/// Resource consumption. Unknown token counts and costs are tracked as
/// unknown, never as zero: `tokens` and `cost_usd` sum only known values and
/// the `*_unknown` flags record that at least one contribution was missing,
/// so the sums are lower bounds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageSummary {
    pub model_requests: u64,
    pub tool_calls: u64,
    pub tokens: u64,
    #[serde(default)]
    pub tokens_unknown: bool,
    pub cost_usd: f64,
    #[serde(default)]
    pub cost_unknown: bool,
}

impl UsageSummary {
    pub fn add(&mut self, other: &UsageSummary) {
        self.model_requests += other.model_requests;
        self.tool_calls += other.tool_calls;
        self.tokens += other.tokens;
        self.tokens_unknown |= other.tokens_unknown;
        self.cost_usd += other.cost_usd;
        self.cost_unknown |= other.cost_unknown;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_reason: Option<RetryReason>,
}

impl OrchestrationError {
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

/// Machine-readable error codes produced by the reducer itself.
pub mod error_codes {
    pub const BUDGET_EXHAUSTED: &str = "BUDGET_EXHAUSTED";
    pub const INDETERMINATE_ATTEMPT: &str = "INDETERMINATE_ATTEMPT";
    pub const MISSING_TRANSITION: &str = "MISSING_TRANSITION";
}

#[derive(Debug, Clone, PartialEq)]
pub enum OrchestrationCommand {
    RecordCheckpoint {
        node_id: OrchestrationNodeId,
        attempt: u32,
        value: Value,
    },
    Start {
        input: Value,
    },
    StepAdmitted {
        node_id: OrchestrationNodeId,
        attempt: u32,
    },
    StepSucceeded {
        node_id: OrchestrationNodeId,
        attempt: u32,
        output: Value,
        details: AttemptDetails,
    },
    StepFailed {
        node_id: OrchestrationNodeId,
        attempt: u32,
        error: OrchestrationError,
        details: AttemptDetails,
    },
    PermissionRequired {
        node_id: OrchestrationNodeId,
        attempt: u32,
        permission_id: String,
    },
    PermissionResolved {
        node_id: OrchestrationNodeId,
        attempt: u32,
        permission_id: String,
        approved: bool,
    },
    InputRequired {
        node_id: OrchestrationNodeId,
        attempt: u32,
        request: InputRequest,
    },
    InputResolved {
        node_id: OrchestrationNodeId,
        attempt: u32,
        request_id: String,
        response: InputResponse,
    },
    RetryStep {
        node_id: OrchestrationNodeId,
    },
    Pause,
    Resume,
    /// Terminate with a runtime-detected error (elapsed budget, persistence
    /// failure at the commit boundary, ...).
    Abort {
        error: OrchestrationError,
    },
    Cancel,
    CancellationCompleted,
    /// Reconcile a state loaded from durable storage. Attempts that were in
    /// flight when the process stopped have unknown side effects, so they are
    /// failed closed instead of blindly re-run.
    Recover,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OrchestrationEffect {
    ExecuteStep {
        node_id: OrchestrationNodeId,
        attempt: u32,
    },
    CancelStep {
        node_id: OrchestrationNodeId,
        attempt: u32,
    },
    Emit(OrchestrationEvent),
    Finish(OrchestrationResult),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OrchestrationEvent {
    TaskProgress {
        node_id: OrchestrationNodeId,
        attempt: u32,
        completed: usize,
        total: usize,
    },
    RunStarted,
    RunRestored,
    RunPaused,
    RunResumed,
    StepReady {
        node_id: OrchestrationNodeId,
    },
    StepStarted {
        node_id: OrchestrationNodeId,
        attempt: u32,
    },
    StepSucceeded {
        node_id: OrchestrationNodeId,
        attempt: u32,
    },
    StepFailed {
        node_id: OrchestrationNodeId,
        attempt: u32,
        error: OrchestrationError,
    },
    StepRetryScheduled {
        node_id: OrchestrationNodeId,
        next_attempt: u32,
        /// The node whose failure triggered the retry (itself, or a verifier).
        triggered_by: OrchestrationNodeId,
    },
    TransitionSelected {
        from: OrchestrationNodeId,
        to: OrchestrationNodeId,
        condition: EdgeCondition,
    },
    PermissionRequested {
        node_id: OrchestrationNodeId,
        attempt: u32,
        permission_id: String,
    },
    PermissionResolved {
        node_id: OrchestrationNodeId,
        attempt: u32,
        permission_id: String,
        approved: bool,
    },
    InputRequested {
        node_id: OrchestrationNodeId,
        attempt: u32,
        request: Box<InputRequest>,
    },
    InputResolved {
        node_id: OrchestrationNodeId,
        attempt: u32,
        request_id: String,
        response: Box<InputResponse>,
    },
    BudgetUpdated {
        usage: UsageSummary,
    },
    RunCompleted,
    RunFailed {
        error: OrchestrationError,
    },
    RunCancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationOutcome {
    Completed,
    Failed,
    Cancelled,
}

/// The provider-neutral public result of a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationResult {
    pub run_id: OrchestrationRunId,
    pub definition_id: OrchestrationDefinitionId,
    pub revision: u64,
    pub status: OrchestrationOutcome,
    pub output: Option<Value>,
    pub usage: UsageSummary,
    pub error: Option<OrchestrationError>,
}

impl OrchestrationResult {
    fn from_state(state: &OrchestrationRunState, status: OrchestrationOutcome) -> Self {
        Self {
            run_id: state.run_id.clone(),
            definition_id: state.definition_id.clone(),
            revision: state.definition_revision,
            status,
            output: state.final_output.clone(),
            usage: state.usage.clone(),
            error: state.error.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TransitionError {}

/// Steps eligible for admission, in deterministic order (topological rank,
/// then node id). Concurrency is one, so at most one step is returned, and
/// none while paused, waiting, or already running.
pub fn ready_steps(
    compiled: &CompiledOrchestration,
    state: &OrchestrationRunState,
) -> Vec<OrchestrationNodeId> {
    if state.status != OrchestrationStatus::Ready || state.paused {
        return Vec::new();
    }
    let mut ready: Vec<_> = state
        .steps
        .iter()
        .filter_map(|(node_id, step)| (step.status == StepStatus::Ready).then_some(node_id.clone()))
        .collect();
    ready.sort_by(|left, right| {
        let rank = |id| {
            compiled
                .topological_rank
                .get(id)
                .copied()
                .unwrap_or(usize::MAX)
        };
        rank(left).cmp(&rank(right)).then_with(|| left.cmp(right))
    });
    ready.truncate(1);
    ready
}

/// Verify that a restored state belongs to exactly this compiled definition.
pub fn check_restorable(
    compiled: &CompiledOrchestration,
    state: &OrchestrationRunState,
) -> Result<(), TransitionError> {
    if state.definition_id != compiled.definition_id
        || state.definition_revision != compiled.revision
    {
        return Err(transition_error(
            "definition_mismatch",
            format!(
                "run was started on {}@{}, not {}@{}",
                state.definition_id,
                state.definition_revision,
                compiled.definition_id,
                compiled.revision
            ),
        ));
    }
    if state.definition_hash != compiled.content_hash {
        return Err(transition_error(
            "definition_hash_mismatch",
            "definition content changed since the run started",
        ));
    }
    if state.steps.len() != compiled.nodes.len()
        || !compiled.nodes.keys().all(|id| state.steps.contains_key(id))
    {
        return Err(transition_error(
            "definition_mismatch",
            "run step set does not match the definition",
        ));
    }
    Ok(())
}

pub fn apply(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    command: OrchestrationCommand,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    if state.status.is_terminal() {
        return Err(transition_error(
            "terminal_run",
            "terminal orchestration runs cannot transition",
        ));
    }
    match command {
        OrchestrationCommand::RecordCheckpoint {
            node_id,
            attempt,
            value,
        } => {
            let completed = value["completed"].as_array().map_or(0, Vec::len);
            let total = value["plan"]["tasks"].as_array().map_or(0, Vec::len);
            active_attempt_mut(compiled, state, &node_id, attempt)?.checkpoint = Some(value);
            Ok(vec![OrchestrationEffect::Emit(
                OrchestrationEvent::TaskProgress {
                    node_id,
                    attempt,
                    completed,
                    total,
                },
            )])
        }
        OrchestrationCommand::Start { input } => start(compiled, state, input),
        OrchestrationCommand::StepAdmitted { node_id, attempt } => {
            admit(compiled, state, node_id, attempt)
        }
        OrchestrationCommand::StepSucceeded {
            node_id,
            attempt,
            output,
            details,
        } => succeed(compiled, state, node_id, attempt, output, details),
        OrchestrationCommand::StepFailed {
            node_id,
            attempt,
            error,
            details,
        } => fail(compiled, state, node_id, attempt, error, details),
        OrchestrationCommand::PermissionRequired {
            node_id,
            attempt,
            permission_id,
        } => permission_required(compiled, state, node_id, attempt, permission_id),
        OrchestrationCommand::PermissionResolved {
            node_id,
            attempt,
            permission_id,
            approved,
        } => permission_resolved(compiled, state, node_id, attempt, permission_id, approved),
        OrchestrationCommand::InputRequired {
            node_id,
            attempt,
            request,
        } => input_required(compiled, state, node_id, attempt, request),
        OrchestrationCommand::InputResolved {
            node_id,
            attempt,
            request_id,
            response,
        } => input_resolved(compiled, state, node_id, attempt, request_id, response),
        OrchestrationCommand::RetryStep { node_id } => retry(compiled, state, node_id),
        OrchestrationCommand::Pause => pause(state),
        OrchestrationCommand::Resume => resume(state),
        OrchestrationCommand::Abort { error } => abort(state, error),
        OrchestrationCommand::Cancel => cancel(state),
        OrchestrationCommand::CancellationCompleted => cancellation_completed(state),
        OrchestrationCommand::Recover => recover(compiled, state),
    }
}

fn start(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    input: Value,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    require_status(state.status, OrchestrationStatus::Created)?;
    state.input = Some(input);
    let step = state
        .steps
        .get_mut(&compiled.entry_node)
        .expect("compiled entry state exists");
    step.status = StepStatus::Ready;
    settle(state);
    Ok(vec![
        OrchestrationEffect::Emit(OrchestrationEvent::RunStarted),
        OrchestrationEffect::Emit(OrchestrationEvent::StepReady {
            node_id: compiled.entry_node.clone(),
        }),
    ])
}

/// Returns the first budget dimension that is exhausted, if any.
fn exhausted_budget(
    compiled: &CompiledOrchestration,
    state: &OrchestrationRunState,
) -> Option<String> {
    let policies = &compiled.definition.policies;
    let usage = &state.usage;
    if policies
        .max_total_attempts
        .is_some_and(|limit| state.total_attempts >= limit)
    {
        return Some(format!(
            "attempt budget of {} exhausted",
            policies.max_total_attempts.unwrap()
        ));
    }
    let checks = [
        (
            "model request",
            policies.max_model_requests,
            usage.model_requests,
        ),
        ("tool call", policies.max_tool_calls, usage.tool_calls),
        ("token", policies.max_tokens, usage.tokens),
    ];
    for (name, limit, used) in checks {
        if let Some(limit) = limit {
            if used >= limit {
                return Some(format!("{name} budget of {limit} exhausted ({used} used)"));
            }
        }
    }
    if let Some(limit) = policies.max_cost_usd {
        if usage.cost_usd >= limit {
            return Some(format!(
                "cost budget of ${limit} exhausted (${} used)",
                usage.cost_usd
            ));
        }
    }
    None
}

fn admit(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
    attempt: u32,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    if state.status != OrchestrationStatus::Ready || state.paused {
        return Err(transition_error(
            "run_not_ready",
            format!("run in status {:?} cannot admit work", state.status),
        ));
    }
    {
        let step = step_mut(compiled, state, &node_id)?;
        if step.status != StepStatus::Ready {
            return Err(transition_error(
                "step_not_ready",
                format!("step {node_id} is not ready"),
            ));
        }
        let expected_attempt = step.attempts.len() as u32 + 1;
        if attempt != expected_attempt {
            return Err(transition_error(
                "invalid_attempt",
                format!("expected attempt {expected_attempt}, got {attempt}"),
            ));
        }
    }
    if let Some(reason) = exhausted_budget(compiled, state) {
        return terminate_failed(
            state,
            OrchestrationError::new(error_codes::BUDGET_EXHAUSTED, reason),
            Vec::new(),
        );
    }
    let step = state
        .steps
        .get_mut(&node_id)
        .expect("validated step exists");
    step.status = StepStatus::Running;
    step.attempts.push(StepAttempt {
        attempt,
        status: AttemptStatus::Running,
        output: None,
        error: None,
        details: AttemptDetails::default(),
    });
    state.total_attempts += 1;
    settle(state);
    Ok(vec![
        OrchestrationEffect::Emit(OrchestrationEvent::StepStarted {
            node_id: node_id.clone(),
            attempt,
        }),
        OrchestrationEffect::ExecuteStep { node_id, attempt },
    ])
}

fn record_usage(
    state: &mut OrchestrationRunState,
    details: &AttemptDetails,
) -> Option<OrchestrationEffect> {
    let usage = &details.usage;
    if *usage == UsageSummary::default() {
        return None;
    }
    state.usage.add(usage);
    Some(OrchestrationEffect::Emit(
        OrchestrationEvent::BudgetUpdated {
            usage: state.usage.clone(),
        },
    ))
}

fn succeed(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
    attempt: u32,
    output: Value,
    details: AttemptDetails,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let is_output = matches!(
        compiled.node(&node_id).map(|node| &node.kind),
        Some(OrchestrationNodeKind::Output(_))
    );
    let usage_effect = record_usage(state, &details);
    {
        let step = active_attempt_mut(compiled, state, &node_id, attempt)?;
        if step.status != StepStatus::Running {
            return Err(transition_error(
                "step_not_running",
                format!("step {node_id} cannot succeed while {:?}", step.status),
            ));
        }
        let current = step.attempts.last_mut().expect("active attempt exists");
        current.status = AttemptStatus::Succeeded;
        current.output = Some(output.clone());
        current.details = details;
        step.status = StepStatus::Succeeded;
        step.output = Some(output.clone());
        step.error = None;
    }
    let mut effects = Vec::from_iter(usage_effect);
    effects.push(OrchestrationEffect::Emit(
        OrchestrationEvent::StepSucceeded {
            node_id: node_id.clone(),
            attempt,
        },
    ));
    if is_output {
        state.final_output = Some(output);
        state.status = OrchestrationStatus::Completed;
        effects.push(OrchestrationEffect::Emit(OrchestrationEvent::RunCompleted));
        effects.push(OrchestrationEffect::Finish(
            OrchestrationResult::from_state(state, OrchestrationOutcome::Completed),
        ));
        return Ok(effects);
    }
    match compiled.outgoing_for(&node_id, EdgeCondition::OnSuccess) {
        Some(edge) => {
            let target = edge.target.clone();
            activate(
                state,
                &node_id,
                &target,
                EdgeCondition::OnSuccess,
                &mut effects,
            )?;
            settle(state);
            Ok(effects)
        }
        None => terminate_failed(
            state,
            OrchestrationError::new(
                error_codes::MISSING_TRANSITION,
                format!("step {node_id} has no success route"),
            ),
            effects,
        ),
    }
}

fn fail(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
    attempt: u32,
    error: OrchestrationError,
    details: AttemptDetails,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let node = compiled
        .node(&node_id)
        .ok_or_else(|| transition_error("unknown_step", format!("unknown step {node_id}")))?;
    let usage_effect = record_usage(state, &details);
    {
        let step = active_attempt_mut(compiled, state, &node_id, attempt)?;
        let current = step.attempts.last_mut().expect("active attempt exists");
        current.status = AttemptStatus::Failed;
        current.error = Some(error.clone());
        current.details = details;
        step.status = StepStatus::Failed;
        step.error = Some(error.clone());
        step.pending_permissions.clear();
        step.pending_input = None;
    }
    let mut effects = Vec::from_iter(usage_effect);
    effects.push(OrchestrationEffect::Emit(OrchestrationEvent::StepFailed {
        node_id: node_id.clone(),
        attempt,
        error: error.clone(),
    }));
    let budget_left = exhausted_budget(compiled, state).is_none();

    // 1. Retry this node under its own policy.
    let own_retry = error
        .retry_reason
        .is_some_and(|reason| node.retry.retry_on.contains(&reason))
        && attempt.saturating_sub(state.steps[&node_id].retry_base) < node.retry.max_attempts
        && budget_left;
    if own_retry {
        let step = state
            .steps
            .get_mut(&node_id)
            .expect("validated step exists");
        step.status = StepStatus::RetryScheduled;
        step.feedback.push(error);
        effects.push(OrchestrationEffect::Emit(
            OrchestrationEvent::StepRetryScheduled {
                node_id: node_id.clone(),
                next_attempt: attempt + 1,
                triggered_by: node_id,
            },
        ));
        settle(state);
        return Ok(effects);
    }

    // 2. A verifier sends work back to its upstream retry target.
    if let (OrchestrationNodeKind::Verify(config), Some(RetryReason::VerificationFailed)) =
        (&node.kind, error.retry_reason)
    {
        if let Some(target_id) = &config.retry_target {
            let target = compiled
                .node(target_id)
                .expect("compiled retry target exists");
            let target_step = &state.steps[target_id];
            let target_attempts = target_step.attempts.len() as u32;
            if target_attempts.saturating_sub(target_step.retry_base) < target.retry.max_attempts
                && budget_left
            {
                send_back(compiled, state, &node_id, target_id, error, &mut effects);
                return Ok(effects);
            }
        }
    }

    // 3. The user asked for changes: an approval sends them back to its
    //    revise target (at most `max_revisions` times), a task queue sends a
    //    plan revision back to the step that wrote the plan. Asking for
    //    changes never uses up the target's failure retries.
    let revise_target = match (&node.kind, error.retry_reason) {
        (OrchestrationNodeKind::Approval(config), Some(RetryReason::ChangesRequested))
            if attempt <= config.max_revisions =>
        {
            config.revise_target()
        }
        (OrchestrationNodeKind::Agent(_), Some(RetryReason::ChangesRequested)) => {
            node.task_plan_source()
        }
        _ => None,
    };
    if let Some(target_id) = revise_target {
        if budget_left && compiled.retry_spans.contains_key(&node_id) {
            let target_step = state.steps.get_mut(target_id).expect("target step exists");
            target_step.retry_base = target_step.attempts.len() as u32;
            send_back(compiled, state, &node_id, target_id, error, &mut effects);
            return Ok(effects);
        }
    }

    // 4. Explicit failure route, else terminal failure.
    if let Some(edge) = compiled.outgoing_for(&node_id, EdgeCondition::OnFailure) {
        let target = edge.target.clone();
        activate(
            state,
            &node_id,
            &target,
            EdgeCondition::OnFailure,
            &mut effects,
        )?;
        settle(state);
        return Ok(effects);
    }
    let error = if budget_left {
        error
    } else {
        OrchestrationError::new(
            error_codes::BUDGET_EXHAUSTED,
            format!("retry denied by budget after: {}", error.message),
        )
    };
    state.failed_step = Some(node_id);
    terminate_failed(state, error, effects)
}

/// Re-run `target_id` with `error` as feedback, resetting every step between
/// it and `from` (the verifier or approval that sent the work back).
fn send_back(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    from: &OrchestrationNodeId,
    target_id: &OrchestrationNodeId,
    error: OrchestrationError,
    effects: &mut Vec<OrchestrationEffect>,
) {
    for span_node in &compiled.retry_spans[from] {
        let step = state.steps.get_mut(span_node).expect("span step exists");
        step.status = StepStatus::Pending;
        step.output = None;
        if span_node != target_id {
            step.checkpoint = None;
        }
    }
    let target_step = state.steps.get_mut(target_id).expect("target step exists");
    let next_attempt = target_step.attempts.len() as u32 + 1;
    target_step.status = StepStatus::RetryScheduled;
    target_step.feedback.push(error);
    effects.push(OrchestrationEffect::Emit(
        OrchestrationEvent::StepRetryScheduled {
            node_id: target_id.clone(),
            next_attempt,
            triggered_by: from.clone(),
        },
    ));
    settle(state);
}

fn activate(
    state: &mut OrchestrationRunState,
    from: &OrchestrationNodeId,
    target: &OrchestrationNodeId,
    condition: EdgeCondition,
    effects: &mut Vec<OrchestrationEffect>,
) -> Result<(), TransitionError> {
    let next = state
        .steps
        .get_mut(target)
        .expect("compiled target state exists");
    if next.status != StepStatus::Pending {
        return Err(transition_error(
            "target_not_pending",
            format!("target step {target} is not pending"),
        ));
    }
    next.status = StepStatus::Ready;
    effects.push(OrchestrationEffect::Emit(
        OrchestrationEvent::TransitionSelected {
            from: from.clone(),
            to: target.clone(),
            condition,
        },
    ));
    effects.push(OrchestrationEffect::Emit(OrchestrationEvent::StepReady {
        node_id: target.clone(),
    }));
    Ok(())
}

fn permission_required(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
    attempt: u32,
    permission_id: String,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let step = active_attempt_mut(compiled, state, &node_id, attempt)?;
    if step.pending_permissions.contains(&permission_id) {
        return Err(transition_error(
            "duplicate_permission",
            format!("permission {permission_id} is already pending"),
        ));
    }
    step.status = StepStatus::WaitingForPermission;
    step.pending_permissions.push(permission_id.clone());
    settle(state);
    Ok(vec![OrchestrationEffect::Emit(
        OrchestrationEvent::PermissionRequested {
            node_id,
            attempt,
            permission_id,
        },
    )])
}

fn permission_resolved(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
    attempt: u32,
    permission_id: String,
    approved: bool,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let step = active_attempt_mut(compiled, state, &node_id, attempt)?;
    let Some(index) = step
        .pending_permissions
        .iter()
        .position(|pending| *pending == permission_id)
    else {
        return Err(transition_error(
            "unknown_permission",
            format!("step {node_id} is not waiting on permission {permission_id}"),
        ));
    };
    // A denial is returned to the agent, as in a direct session; the step
    // only fails if the agent cannot continue without the tool.
    step.pending_permissions.remove(index);
    if step.pending_permissions.is_empty() {
        step.status = StepStatus::Running;
    }
    settle(state);
    Ok(vec![OrchestrationEffect::Emit(
        OrchestrationEvent::PermissionResolved {
            node_id,
            attempt,
            permission_id,
            approved,
        },
    )])
}

fn input_required(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
    attempt: u32,
    request: InputRequest,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let step = active_attempt_mut(compiled, state, &node_id, attempt)?;
    if step.pending_input.is_some() {
        return Err(transition_error(
            "input_pending",
            format!("step {node_id} is already waiting for an answer"),
        ));
    }
    if request.decisions.is_empty() {
        return Err(transition_error(
            "invalid_input_request",
            "a question needs at least one possible answer",
        ));
    }
    step.status = StepStatus::WaitingForInput;
    step.pending_input = Some(request.clone());
    settle(state);
    Ok(vec![OrchestrationEffect::Emit(
        OrchestrationEvent::InputRequested {
            node_id,
            attempt,
            request: Box::new(request),
        },
    )])
}

fn input_resolved(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
    attempt: u32,
    request_id: String,
    response: InputResponse,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let step = active_attempt_mut(compiled, state, &node_id, attempt)?;
    let Some(request) = step
        .pending_input
        .as_ref()
        .filter(|request| request.id == request_id)
    else {
        return Err(transition_error(
            "unknown_input",
            format!("step {node_id} is not waiting on question {request_id}"),
        ));
    };
    let Some(decision) = request
        .decisions
        .iter()
        .find(|decision| decision.id == response.decision)
    else {
        return Err(transition_error(
            "invalid_decision",
            format!(
                "{} is not one of the answers to question {request_id}",
                response.decision
            ),
        ));
    };
    if decision.requires_text
        && !matches!(response.text.as_deref(), Some(text) if !text.trim().is_empty())
    {
        return Err(transition_error(
            "missing_text",
            format!("the answer {} needs text", decision.id),
        ));
    }
    step.pending_input = None;
    step.status = if step.pending_permissions.is_empty() {
        StepStatus::Running
    } else {
        StepStatus::WaitingForPermission
    };
    settle(state);
    Ok(vec![OrchestrationEffect::Emit(
        OrchestrationEvent::InputResolved {
            node_id,
            attempt,
            request_id,
            response: Box::new(response),
        },
    )])
}

fn retry(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let step = step_mut(compiled, state, &node_id)?;
    if step.status != StepStatus::RetryScheduled {
        return Err(transition_error(
            "retry_not_scheduled",
            "step has no scheduled retry",
        ));
    }
    step.status = StepStatus::Ready;
    settle(state);
    Ok(vec![OrchestrationEffect::Emit(
        OrchestrationEvent::StepReady { node_id },
    )])
}

fn pause(state: &mut OrchestrationRunState) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    if state.paused {
        return Err(transition_error("already_paused", "run is already paused"));
    }
    if matches!(
        state.status,
        OrchestrationStatus::Created | OrchestrationStatus::Cancelling
    ) {
        return Err(transition_error(
            "invalid_run_status",
            format!("cannot pause a run in status {:?}", state.status),
        ));
    }
    state.paused = true;
    settle(state);
    Ok(vec![OrchestrationEffect::Emit(
        OrchestrationEvent::RunPaused,
    )])
}

fn resume(state: &mut OrchestrationRunState) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    if !state.paused {
        return Err(transition_error("not_paused", "run is not paused"));
    }
    state.paused = false;
    settle(state);
    Ok(vec![OrchestrationEffect::Emit(
        OrchestrationEvent::RunResumed,
    )])
}

fn abort(
    state: &mut OrchestrationRunState,
    error: OrchestrationError,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let mut effects = Vec::new();
    for (node_id, step) in &mut state.steps {
        if step.status.is_active() {
            let attempt = step.attempts.last_mut().expect("active step has attempt");
            attempt.status = AttemptStatus::Cancelled;
            effects.push(OrchestrationEffect::CancelStep {
                node_id: node_id.clone(),
                attempt: attempt.attempt,
            });
            step.status = StepStatus::Cancelled;
            step.pending_permissions.clear();
            step.pending_input = None;
        }
    }
    terminate_failed(state, error, effects)
}

fn cancel(state: &mut OrchestrationRunState) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    if state.status == OrchestrationStatus::Cancelling {
        return Err(transition_error(
            "already_cancelling",
            "run is already cancelling",
        ));
    }
    state.status = OrchestrationStatus::Cancelling;
    let mut effects = Vec::new();
    for (node_id, step) in &mut state.steps {
        match step.status {
            StepStatus::Running
            | StepStatus::WaitingForPermission
            | StepStatus::WaitingForInput => {
                let attempt = step.attempts.last_mut().expect("active step has attempt");
                attempt.status = AttemptStatus::Cancelled;
                effects.push(OrchestrationEffect::CancelStep {
                    node_id: node_id.clone(),
                    attempt: attempt.attempt,
                });
                step.status = StepStatus::Cancelled;
                step.pending_permissions.clear();
                step.pending_input = None;
            }
            StepStatus::Ready | StepStatus::RetryScheduled => step.status = StepStatus::Cancelled,
            StepStatus::Pending => step.status = StepStatus::Skipped,
            _ => {}
        }
    }
    if effects.is_empty() {
        return cancellation_completed(state);
    }
    Ok(effects)
}

fn cancellation_completed(
    state: &mut OrchestrationRunState,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    require_status(state.status, OrchestrationStatus::Cancelling)?;
    state.status = OrchestrationStatus::Cancelled;
    Ok(vec![
        OrchestrationEffect::Emit(OrchestrationEvent::RunCancelled),
        OrchestrationEffect::Finish(OrchestrationResult::from_state(
            state,
            OrchestrationOutcome::Cancelled,
        )),
    ])
}

fn recover(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    check_restorable(compiled, state)?;
    let mut effects = vec![OrchestrationEffect::Emit(OrchestrationEvent::RunRestored)];
    if state.status == OrchestrationStatus::Cancelling {
        for step in state.steps.values_mut() {
            if step.status.is_active() {
                step.status = StepStatus::Cancelled;
            }
        }
        effects.extend(cancellation_completed(state)?);
        return Ok(effects);
    }
    // Input, verify, approval and output nodes are pure functions of
    // persisted state, so an interrupted attempt is simply abandoned and the
    // step re-admitted (an unanswered approval is asked again). Only agent
    // attempts have side effects of unknown extent.
    let (indeterminate, replayable): (Vec<_>, Vec<_>) = state
        .steps
        .iter()
        .filter(|(_, step)| step.status.is_active())
        .map(|(id, _)| id.clone())
        .partition(|id| {
            matches!(
                compiled.node(id).map(|node| &node.kind),
                Some(OrchestrationNodeKind::Agent(_) | OrchestrationNodeKind::Subflow(_))
            )
        });
    for node_id in &replayable {
        let step = state.steps.get_mut(node_id).expect("listed step exists");
        let attempt = step.attempts.last_mut().expect("active step has attempt");
        attempt.status = AttemptStatus::Cancelled;
        step.status = StepStatus::Ready;
        step.pending_permissions.clear();
        step.pending_input = None;
        effects.push(OrchestrationEffect::Emit(OrchestrationEvent::StepReady {
            node_id: node_id.clone(),
        }));
    }
    if indeterminate.is_empty() {
        settle(state);
        return Ok(effects);
    }
    let error = OrchestrationError::new(
        error_codes::INDETERMINATE_ATTEMPT,
        format!(
            "step(s) {} were in flight when the run stopped; side effects are unknown",
            indeterminate
                .iter()
                .map(OrchestrationNodeId::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    for node_id in &indeterminate {
        let step = state.steps.get_mut(node_id).expect("listed step exists");
        let attempt = step.attempts.last_mut().expect("active step has attempt");
        attempt.status = AttemptStatus::Failed;
        attempt.error = Some(error.clone());
        step.status = StepStatus::Failed;
        step.error = Some(error.clone());
        step.pending_permissions.clear();
        step.pending_input = None;
    }
    terminate_failed(state, error, effects)
}

/// Derive the non-terminal run status from step states and the pause flag.
fn settle(state: &mut OrchestrationRunState) {
    if state.status.is_terminal() || state.status == OrchestrationStatus::Cancelling {
        return;
    }
    state.status = match state.active_step().map(|(_, step)| step.status) {
        Some(StepStatus::WaitingForPermission) => OrchestrationStatus::WaitingForPermission,
        Some(StepStatus::WaitingForInput) => OrchestrationStatus::WaitingForInput,
        Some(_) => OrchestrationStatus::Running,
        None if state.paused => OrchestrationStatus::Paused,
        None => OrchestrationStatus::Ready,
    };
}

fn step_mut<'a>(
    compiled: &CompiledOrchestration,
    state: &'a mut OrchestrationRunState,
    node_id: &OrchestrationNodeId,
) -> Result<&'a mut StepRun, TransitionError> {
    if compiled.node(node_id).is_none() {
        return Err(transition_error(
            "unknown_step",
            format!("unknown step {node_id}"),
        ));
    }
    state.steps.get_mut(node_id).ok_or_else(|| {
        transition_error("missing_step_state", format!("missing state for {node_id}"))
    })
}

/// The step, if `attempt` is its in-flight attempt. Rejects completions from
/// stale attempts so an old delegated run cannot complete a newer retry.
fn active_attempt_mut<'a>(
    compiled: &CompiledOrchestration,
    state: &'a mut OrchestrationRunState,
    node_id: &OrchestrationNodeId,
    attempt: u32,
) -> Result<&'a mut StepRun, TransitionError> {
    let step = step_mut(compiled, state, node_id)?;
    if !step.status.is_active() {
        return Err(transition_error(
            "step_not_running",
            format!("step {node_id} is not running"),
        ));
    }
    if step.attempts.last().map(|value| value.attempt) != Some(attempt) {
        return Err(transition_error(
            "stale_attempt",
            format!("attempt {attempt} is not active"),
        ));
    }
    Ok(step)
}

fn require_status(
    actual: OrchestrationStatus,
    expected: OrchestrationStatus,
) -> Result<(), TransitionError> {
    if actual == expected {
        Ok(())
    } else {
        Err(transition_error(
            "invalid_run_status",
            format!("expected run status {expected:?}, got {actual:?}"),
        ))
    }
}

fn terminate_failed(
    state: &mut OrchestrationRunState,
    error: OrchestrationError,
    mut effects: Vec<OrchestrationEffect>,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    state.status = OrchestrationStatus::Failed;
    state.error = Some(error.clone());
    for step in state.steps.values_mut() {
        match step.status {
            StepStatus::Pending => step.status = StepStatus::Skipped,
            StepStatus::Ready | StepStatus::RetryScheduled => step.status = StepStatus::Cancelled,
            _ => {}
        }
    }
    effects.push(OrchestrationEffect::Emit(OrchestrationEvent::RunFailed {
        error,
    }));
    effects.push(OrchestrationEffect::Finish(
        OrchestrationResult::from_state(state, OrchestrationOutcome::Failed),
    ));
    Ok(effects)
}

fn transition_error(code: &'static str, message: impl Into<String>) -> TransitionError {
    TransitionError {
        code,
        message: message.into(),
    }
}
