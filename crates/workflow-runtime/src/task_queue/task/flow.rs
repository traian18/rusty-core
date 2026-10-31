//! Doing a task by running one of the queue's flows.
//!
//! The flow judges its own result, so there is no separate review; a flow that fails fails the
//! task, which the failure policy then handles.

use serde_json::{json, Value};

use crate::orchestration::{AgentExecutionError, SubflowRequest};
use crate::task_queue::model::{Task, TaskOutcome};
use crate::task_queue::queue::Queue;
use rusty_workflow_core::{OrchestrationNodeId, SubflowTarget};

impl Queue<'_> {
    /// The flow the task names, when it names one this queue offers.
    pub(super) fn flow_target(&self, task: &Task) -> Option<SubflowTarget> {
        let name = task.flow.as_deref()?;
        self.settings.flows.get(name).cloned()
    }

    pub(super) async fn run_task_flow(
        &mut self,
        task: &Task,
        target: &SubflowTarget,
        record: bool,
    ) -> Result<TaskOutcome, AgentExecutionError> {
        let subflows = self.executor.subflows.as_ref().ok_or_else(|| {
            AgentExecutionError::new(
                "subflows_unavailable",
                "this host cannot run flows inside flows",
            )
        })?;
        let request = SubflowRequest {
            run_id: self.request.run_id.clone(),
            node_id: OrchestrationNodeId::new(format!("{}.{}", self.request.node_id, task.id)),
            attempt: self.request.attempt,
            target: target.clone(),
            input: self.flow_input(task),
            depth: self.request.subflow_depth,
            options: self.request.run_options.clone(),
        };
        match subflows.execute(request, self.context).await {
            Ok(output) => self.finish_flow(task, output, record).await,
            // Stopping the run is not a failed task.
            Err(error) if error.code == "cancelled" => Err(error),
            Err(error) => Ok(TaskOutcome::Failed(format!(
                "Task {} could not be done by the flow {}: {}",
                task.id,
                task.flow.as_deref().unwrap_or(""),
                error.message
            ))),
        }
    }

    /// The step's own input, plus what the flow needs to know about the task.
    fn flow_input(&self, task: &Task) -> Value {
        let mut input = json!({
            "request": self.request.input.get("request"),
            "context": self.request.input.get("context"),
            "requirements": self.plan.requirements,
            "task": task,
            "completed_tasks": self.progress.completed,
        });
        if let (Some(fields), Some(target)) =
            (self.request.input.as_object(), input.as_object_mut())
        {
            for (name, value) in fields {
                target.entry(name.clone()).or_insert_with(|| value.clone());
            }
        }
        input
    }

    async fn finish_flow(
        &mut self,
        task: &Task,
        output: Value,
        record: bool,
    ) -> Result<TaskOutcome, AgentExecutionError> {
        if record {
            self.progress.completed.push(json!({
                "task_id": task.id,
                "flow": task.flow,
                "result": output,
            }));
        }
        self.progress.current = None;
        self.checkpoint().await?;
        Ok(TaskOutcome::Accepted)
    }
}
