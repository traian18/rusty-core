//! Task queue planning, review, and failure configuration.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    AgentNodeConfig, OrchestrationNode, OrchestrationNodeId, OrchestrationNodeKind, OutputBinding,
    SubflowTarget,
};

/// Runs a plan's tasks one at a time, each in a fresh builder session
/// followed by a read-only review, checkpointing every accepted task. The
/// step's own output is the list of completed tasks (always an object,
/// whatever the step's response format).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskQueueConfig {
    /// Pointer to a typed requirement/task plan in the bound input; its first
    /// segment names the input binding the plan comes from.
    pub plan_pointer: String,
    pub review_profile: crate::behavior::ProfileRef,
    pub review_instructions: String,
    /// When set, a read-only planner turns each task into a detailed step
    /// plan before it is built; the builder and the reviewer work from it.
    /// The plan the queue reads can then stay general.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_planner: Option<TaskPlannerConfig>,
    pub max_repairs: u32,
    /// What happens when a task cannot be completed: its repairs ran out, or
    /// it needs something only the user can give.
    #[serde(default)]
    pub on_task_failure: TaskFailurePolicy,
    /// Named flows a planned task may run instead of the builder and
    /// reviewer, by setting its `flow` to the name: gathering more
    /// information by research, fixing a bug found on the way, and so on. A
    /// flow judges its own result; a failed one fails the task.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub flows: BTreeMap<String, SubflowTarget>,
}

/// The step that plans one task of a queue in detail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskPlannerConfig {
    pub profile: crate::behavior::ProfileRef,
    pub instructions: String,
}

impl TaskQueueConfig {
    /// The profiles the queue's own sessions run under: the reviewer's and,
    /// when there is one, the task planner's.
    pub fn profiles(&self) -> impl Iterator<Item = &crate::behavior::ProfileRef> {
        std::iter::once(&self.review_profile)
            .chain(self.task_planner.as_ref().map(|planner| &planner.profile))
    }

    /// The input binding the plan is read from.
    pub fn plan_binding(&self) -> Option<&str> {
        self.plan_pointer
            .strip_prefix('/')?
            .split('/')
            .next()
            .filter(|segment| !segment.is_empty())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskFailurePolicy {
    /// Ask the user: try again (with their notes), skip the task, revise the
    /// plan, or stop.
    #[default]
    Ask,
    /// End the step; completed tasks stay checkpointed for a continuation.
    Stop,
    /// Record the task as skipped and go on with the next one.
    Skip,
}

impl OrchestrationNode {
    /// The step that produced a task queue's plan, when the plan is bound
    /// from a step's output. Asking to revise the plan re-runs that step.
    pub fn task_plan_source(&self) -> Option<&OrchestrationNodeId> {
        let OrchestrationNodeKind::Agent(AgentNodeConfig {
            task_queue: Some(queue),
            ..
        }) = &self.kind
        else {
            return None;
        };
        let target = queue.plan_binding()?;
        self.input_bindings
            .iter()
            .find(|binding| binding.target == target)
            .and_then(|binding| match &binding.source {
                OutputBinding::NodeOutput { node_id, .. } => Some(node_id),
                OutputBinding::RunInput { .. } => None,
            })
    }
}
