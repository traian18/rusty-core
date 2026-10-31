//! What goes into the reducer (commands), what comes out (effects, events), and how a run ends.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::definition::{
    EdgeCondition, OrchestrationDefinitionId, OrchestrationNodeId, OrchestrationRunId, RetryReason,
};

use super::{AttemptDetails, InputRequest, InputResponse, OrchestrationRunState, UsageSummary};

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
    pub(in crate::state) fn from_state(
        state: &OrchestrationRunState,
        status: OrchestrationOutcome,
    ) -> Self {
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
