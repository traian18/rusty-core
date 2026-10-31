//! What an agent step is asked to do, what it returns, and how it fails.

use rusty_workflow_core::{
    AgentContextMode, OrchestrationError, OrchestrationNodeId, OrchestrationRunId, RetryReason,
    StructuredOutputMode,
};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct AgentStepRequest {
    pub task_queue: Option<rusty_workflow_core::TaskQueueConfig>,
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
    pub profile: Option<rusty_agent_core::behavior::ProfileRef>,
    /// Why earlier attempts were rejected (validation or verification
    /// errors). Untrusted data, like `input`.
    pub feedback: Vec<OrchestrationError>,
    /// A task queue may offer to revise the plan: failing with
    /// `changes_requested` re-runs the step that wrote it.
    pub plan_revisable: bool,
    /// How deep a flow this step starts would be, and the run's options: for
    /// the flows a task queue runs.
    pub subflow_depth: u32,
    pub run_options: rusty_workflow_core::RunOptions,
    /// What this step's previous attempt did and handed on, when it runs
    /// again. Untrusted data, like `input`.
    pub previous_attempt: Option<String>,
    /// The records of the other steps its `context_access` lets it see.
    /// Untrusted data, like `input`.
    pub journal: Option<String>,
    /// Whether, and how, the step may ask for research.
    pub research: rusty_workflow_core::ResearchPolicy,
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
