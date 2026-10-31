#![forbid(unsafe_code)]
//! Deterministic orchestration definitions, compilation, and run-state transitions.
//!
//! This module contains no I/O. Runtime adapters execute emitted effects and feed
//! their outcomes back as commands, allowing future node implementations and UI
//! editors to share the same stable graph contract.

mod compiler;
mod default;
mod definition;
mod registry;
mod state;
mod task_plan;

pub use compiler::{
    compile, CompiledOrchestration, DefinitionIssue, DefinitionValidationError,
    SUPPORTED_SCHEMA_KEYWORDS,
};
pub use default::default_orchestration_definition;
pub use definition::{
    AgentContextMode, AgentNodeConfig, ApprovalNodeConfig, ContextAccess, DefinitionStatus,
    EdgeCondition, InputBinding, InputNodeConfig, OrchestrationDefinition,
    OrchestrationDefinitionId, OrchestrationEdge, OrchestrationEdgeId, OrchestrationNode,
    OrchestrationNodeId, OrchestrationNodeKind, OrchestrationPolicies, OrchestrationRunId,
    OutputBinding, OutputContract, OutputNodeConfig, ResearchPolicy, RetryPolicy, RetryReason,
    SchemaReference, StructuredOutputMode, SubflowNodeConfig, SubflowTarget, TaskFailurePolicy,
    TaskPlannerConfig, TaskQueueConfig, ToolScope, VerificationCheck, VerifyNodeConfig,
    MAX_REVISIONS, MAX_STEP_ATTEMPTS, MAX_SUBFLOW_DEPTH, ORCHESTRATION_SCHEMA_VERSION,
};
pub use registry::{DefinitionRef, DefinitionRegistry, RegistryError};
pub use state::{
    apply, check_restorable, error_codes, ready_steps, AttemptDetails, AttemptStatus, CommandRun,
    DelegatedRunRef, Evidence, InputDecision, InputRequest, InputResponse, OrchestrationCommand,
    OrchestrationEffect, OrchestrationError, OrchestrationEvent, OrchestrationOutcome,
    OrchestrationResult, OrchestrationRunState, OrchestrationStatus, Responder, RunOptions,
    StepActions, StepAttempt, StepRun, StepStatus, TransitionError, UsageSummary,
};
pub use task_plan::{
    is_task_plan_schema, task_plan_schema, TASK_PLAN_SCHEMA_ID, TASK_PLAN_SCHEMA_REVISION,
};

#[cfg(test)]
mod tests;
