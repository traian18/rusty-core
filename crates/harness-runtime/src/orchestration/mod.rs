//! Runtime adapter for deterministic orchestration definitions.
//!
//! The graph, compiler, and state reducer live in `harness-core`. This module
//! performs the I/O: it executes steps, delegates agent steps to an
//! [`AgentStepExecutor`] (normally an isolated session, see
//! [`crate::session_agent_executor`]), validates structured boundaries,
//! persists every transition, and feeds outcomes back into the reducer.

mod runner;
mod schema;
pub(crate) mod steps;
mod store;
mod subflow;

pub use runner::{
    ControlError, CorrelatedAgentEvent, OrchestrationHandle, OrchestrationRunOutput,
    OrchestrationRunner, OrchestrationRuntimeError, OrchestrationUpdate, StepCorrelation,
};
pub use schema::{
    BasicSchemaValidator, InMemorySchemaResolver, SchemaResolutionError, SchemaResolver,
    SchemaValidationError, SchemaValidator,
};
pub use steps::{
    AgentExecutionError, AgentStepExecutor, AgentStepOutput, AgentStepRequest, ArtifactResolver,
    InputAnswer, PermissionResolution, ReferenceArtifactResolver, StepContext, StepSignal,
    SubflowExecutor, SubflowRequest, WorkspaceArtifactResolver, CHANGES_REQUESTED,
};
pub use store::{
    FileOrchestrationStore, InMemoryOrchestrationStore, OrchestrationEventEnvelope,
    OrchestrationSnapshot, OrchestrationStore, OrchestrationStoreError,
};
pub use subflow::drive_child;

#[cfg(test)]
mod tests;
