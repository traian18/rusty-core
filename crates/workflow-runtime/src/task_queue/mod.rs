//! A persisted, serial task queue. Every task gets fresh builder and reviewer sessions; a retry
//! retains completed tasks, never just a prose handoff.
//!
//! One planned step carries the queue. It reads the plan the planner made, resumes from the step's
//! checkpoint, folds in what the steps after it and the user said, then works through the tasks one
//! at a time. A task is planned in detail (optionally), built under a watchdog, typechecked by the
//! workflow and judged by a read-only reviewer; a task that cannot be completed goes to the queue's
//! failure policy.

mod brief;
mod checks;
mod failure;
mod feedback;
mod ids;
mod model;
mod plan;
mod queue;
mod recovery;
mod replan;
mod resume;
mod scope;
pub(crate) mod snapshot;
mod split;
mod summary;
mod task;
mod walk;

use std::collections::HashMap;

use rusty_protocol::{ids::AgentId, usage::AgentUsageMetrics};

use crate::orchestration::{AgentExecutionError, AgentStepOutput, AgentStepRequest, StepContext};
use crate::session_agent_executor::IsolatedSessionAgentExecutor;
use ids::id_key;
use model::{Plan, Progress, Task};
use queue::Queue;
use scope::is_read_only;

impl IsolatedSessionAgentExecutor {
    /// Runs a step that carries a task queue: executes the planned tasks one at a time, each with
    /// fresh builder and reviewer sessions, and returns the combined output. Chosen by the
    /// executor when the step has a task queue; every other step goes through
    /// `run_single_agent_step`.
    pub(crate) async fn run_planned_task_queue(
        &self,
        request: AgentStepRequest,
        context: &mut StepContext,
        usage: &mut HashMap<AgentId, AgentUsageMetrics>,
    ) -> Result<AgentStepOutput, AgentExecutionError> {
        let settings = request
            .task_queue
            .as_ref()
            .ok_or_else(|| plan::invalid("this step has no task queue"))?;
        self.prune_old_checkpoints(&request).await;
        let plan = resume::read_plan(&request, settings)?;
        let mut progress = resume::read_progress(&request, &plan)?;
        feedback::absorb(&request, &plan, &mut progress);
        let mut queue = Queue {
            executor: self,
            request: &request,
            settings,
            plan: &plan,
            context,
            usage,
            progress,
        };
        queue.checkpoint().await?;
        queue.work_through().await?;
        Ok(summary::output(&plan, &queue.progress))
    }
}

impl IsolatedSessionAgentExecutor {
    /// Drops the checkpoints of runs older than the default retention, so a long-lived workspace
    /// does not collect them without end. Best effort and off the async threads; this run's own
    /// checkpoints are kept.
    async fn prune_old_checkpoints(&self, request: &AgentStepRequest) {
        let root = self.workspace_root();
        let run = request.run_id.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            snapshot::prune_checkpoints(&root, snapshot::DEFAULT_MAX_AGE_DAYS, Some(&run))
        })
        .await;
    }
}

/// A planning step's watchdog: it nudges a planner that keeps reading.
fn planner_watchdog() -> std::sync::Arc<crate::session_agent_executor::watchdog::Watchdog> {
    crate::session_agent_executor::watchdog::Watchdog::new(
        crate::session_agent_executor::watchdog::Limits::planner(),
    )
}
