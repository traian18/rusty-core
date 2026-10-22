//! The async orchestration runner.
//!
//! One spawned task owns a run. It feeds commands to the deterministic
//! reducer in `harness-core`, commits every resulting transition to the
//! [`OrchestrationStore`] *before* publishing events or dispatching effects,
//! executes at most one step at a time, and exposes control through an
//! [`OrchestrationHandle`].
//!
//! This file holds the public types. Behaviour is split by responsibility:
//! `builder` and `launch` configure and start runs, `retry` restarts a failed
//! one, `handle` is the caller's side, `run_loop` owns a running run, and
//! `step_job` executes one step attempt.

mod builder;
mod handle;
mod launch;
mod retry;
mod run_loop;
mod step_job;

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use harness_core::orchestration::{
    CompiledOrchestration, DelegatedRunRef, OrchestrationDefinitionId, OrchestrationNodeId,
    OrchestrationResult, OrchestrationRunId, OrchestrationRunState, OrchestrationStatus,
    RunOptions, TransitionError,
};
use harness_protocol::events::AgentEventEnvelope;
use thiserror::Error;

use super::{
    schema::{SchemaResolver, SchemaValidator},
    steps::{AgentStepExecutor, ArtifactResolver, SubflowExecutor},
    store::{OrchestrationEventEnvelope, OrchestrationStore, OrchestrationStoreError},
};

pub use handle::OrchestrationHandle;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Everything needed to attribute an agent event to a workflow step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepCorrelation {
    pub run_id: OrchestrationRunId,
    pub definition_id: OrchestrationDefinitionId,
    pub revision: u64,
    pub node_id: OrchestrationNodeId,
    pub attempt: u32,
    pub delegated: DelegatedRunRef,
}

#[derive(Debug, Clone)]
pub struct CorrelatedAgentEvent {
    pub correlation: StepCorrelation,
    pub envelope: AgentEventEnvelope,
}

/// What subscribers receive: committed orchestration events, and live agent
/// events from delegated attempts tagged with their step correlation.
#[derive(Debug, Clone)]
pub enum OrchestrationUpdate {
    Event(OrchestrationEventEnvelope),
    Agent(Box<CorrelatedAgentEvent>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrchestrationRunOutput {
    pub state: OrchestrationRunState,
    pub result: OrchestrationResult,
    /// Events committed by this runner invocation (a resumed run starts
    /// with `RunRestored`; the store holds the full log).
    pub events: Vec<OrchestrationEventEnvelope>,
}

impl OrchestrationRunOutput {
    /// Delegated session/agent/run for every agent attempt, including failed
    /// and cancelled ones.
    pub fn agent_correlations(&self) -> Vec<StepCorrelation> {
        self.state
            .steps
            .iter()
            .flat_map(|(node_id, step)| {
                step.attempts.iter().filter_map(move |attempt| {
                    attempt
                        .details
                        .delegated
                        .clone()
                        .map(|delegated| StepCorrelation {
                            run_id: self.state.run_id.clone(),
                            definition_id: self.state.definition_id.clone(),
                            revision: self.state.definition_revision,
                            node_id: node_id.clone(),
                            attempt: attempt.attempt,
                            delegated,
                        })
                })
            })
            .collect()
    }
}

#[derive(Debug, Error)]
pub enum OrchestrationRuntimeError {
    #[error("orchestration transition failed ({code}): {message}")]
    Transition { code: &'static str, message: String },
    #[error("persistence failed at the commit boundary; run halted: {0}")]
    Persistence(#[from] OrchestrationStoreError),
    #[error("definition references tools that are not registered: {0:?}")]
    UnknownTools(Vec<String>),
    #[error("resuming a run requires an orchestration store")]
    NoStore,
    #[error("run {0} has no snapshot to resume from")]
    RunNotFound(OrchestrationRunId),
    #[error("run {0} already finished")]
    RunFinished(OrchestrationRunId),
    #[error("cannot restore run: {0}")]
    Restore(String),
    #[error("orchestration stalled in status {0:?} with no runnable step")]
    Stalled(OrchestrationStatus),
    #[error("orchestration task ended abnormally: {0}")]
    TaskFailed(String),
}

impl From<TransitionError> for OrchestrationRuntimeError {
    fn from(error: TransitionError) -> Self {
        Self::Transition {
            code: error.code,
            message: error.message,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ControlError {
    #[error("{0}")]
    Rejected(String),
    #[error("the orchestration run is no longer accepting commands")]
    Closed,
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

/// Executes one compiled definition. Cheap to clone.
#[derive(Clone)]
pub struct OrchestrationRunner {
    compiled: Arc<CompiledOrchestration>,
    agent: Arc<dyn AgentStepExecutor>,
    schemas: Arc<dyn SchemaResolver>,
    validator: Arc<dyn SchemaValidator>,
    artifacts: Arc<dyn ArtifactResolver>,
    store: Option<Arc<dyn OrchestrationStore>>,
    available_tools: Option<Arc<Vec<String>>>,
    options: RunOptions,
    subflows: Option<Arc<dyn SubflowExecutor>>,
    /// How deep this run is in a chain of flows calling flows.
    depth: u32,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}
