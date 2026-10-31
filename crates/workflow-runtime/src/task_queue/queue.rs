//! The queue's working state while one step runs.
//!
//! A queue run owns its progress and borrows everything else: the executor that starts the
//! builder, reviewer and planner sessions, the step's request and plan, and the run's context.
//! Progress is checkpointed after every change, so a stopped run resumes where it was.

use std::collections::HashMap;

use rusty_protocol::{ids::AgentId, usage::AgentUsageMetrics};
use rusty_workflow_core::TaskQueueConfig;

use super::model::{Plan, Progress};
use crate::orchestration::{AgentExecutionError, AgentStepRequest, StepContext};
use crate::session_agent_executor::IsolatedSessionAgentExecutor;

pub(super) struct Queue<'a> {
    pub(super) executor: &'a IsolatedSessionAgentExecutor,
    pub(super) request: &'a AgentStepRequest,
    pub(super) settings: &'a TaskQueueConfig,
    pub(super) plan: &'a Plan,
    pub(super) context: &'a mut StepContext,
    pub(super) usage: &'a mut HashMap<AgentId, AgentUsageMetrics>,
    pub(super) progress: Progress,
}

impl Queue<'_> {
    /// Saves the progress with the run, so a resumed run continues from it.
    pub(super) async fn checkpoint(&mut self) -> Result<(), AgentExecutionError> {
        let saved = serde_json::to_value(&self.progress)
            .map_err(|error| AgentExecutionError::new("invalid_checkpoint", error.to_string()))?;
        self.context.checkpoint(saved).await
    }
}
