//! Isolated session-backed execution for orchestration agent nodes.
//!
//! Every attempt of an agent step runs in a fresh session built from the parent's host dependencies
//! (`session`), told what to do by a prompt (`prompt`), and followed until it answers
//! (`conversation`). Tool permission requests are surfaced to the orchestration runner, which moves
//! the step to `WaitingForPermission`, and the decisions it relays are applied to this session, so
//! the existing approval flow is reused as-is.

mod answer_text;
mod conversation;
mod failure;
mod profile;
mod prompt;
mod rate_limit;
mod reply;
mod research;
mod session;
mod toolset;
mod usage;
pub(crate) mod watchdog;

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use rusty_agent_core::behavior::ProfileRegistry;
use rusty_protocol::{ids::AgentId, usage::AgentUsageMetrics};
use rusty_runtime::session_runtime::SessionRuntime;

use crate::orchestration::{
    AgentExecutionError, AgentStepExecutor, AgentStepOutput, AgentStepRequest, StepContext,
};
use conversation::Conversation;
use prompt::PromptOptions;
use watchdog::Watchdog;

pub use research::RESEARCH_TOOL;

/// Executes every orchestration attempt in a fresh session.
///
/// The new session gets a fresh transcript, agent ID, run ID, cancellation tree, scoped backend
/// request view, and scoped executor registry. From the parent it inherits only host
/// dependencies — backend, workspace, integrations — plus the parent's active execution
/// parameters and the permission policy of each tool it is allowed to use.
pub struct IsolatedSessionAgentExecutor {
    parent: Arc<SessionRuntime>,
    profiles: Option<Arc<ProfileRegistry>>,
    /// Runs the flows a task queue's tasks name.
    pub(crate) subflows: Option<Arc<dyn crate::orchestration::SubflowExecutor>>,
}

impl IsolatedSessionAgentExecutor {
    pub fn new(parent: Arc<SessionRuntime>) -> Self {
        Self {
            parent,
            profiles: None,
            subflows: None,
        }
    }

    /// Lets a task queue run the flows its tasks name.
    pub fn with_subflows(
        mut self,
        subflows: Arc<dyn crate::orchestration::SubflowExecutor>,
    ) -> Self {
        self.subflows = Some(subflows);
        self
    }

    /// Resolve steps' `profile` references against `registry`. Without it a step naming a
    /// profile fails.
    pub fn with_profiles(mut self, registry: Arc<ProfileRegistry>) -> Self {
        self.profiles = Some(registry);
        self
    }

    /// The folder the steps work in.
    pub(crate) fn workspace_root(&self) -> std::path::PathBuf {
        self.parent.workspace.root().to_path_buf()
    }

    /// One of the host's tools, for the workflow's own calls (no session).
    pub(crate) fn host_tool(&self, name: &str) -> Option<Arc<dyn rusty_tool_api::ToolExecutor>> {
        self.parent.tool_registry.get_executor(name)
    }

    /// Runs one workflow agent step to completion in its own isolated session and returns its
    /// output. Called once per step, and once per task by the task queue.
    pub(crate) async fn run_single_agent_step(
        &self,
        request: AgentStepRequest,
        context: &mut StepContext,
        usage: &mut HashMap<AgentId, AgentUsageMetrics>,
    ) -> Result<AgentStepOutput, AgentExecutionError> {
        self.run_agent_step(request, context, usage, None).await
    }

    /// Runs one agent step whose tool calls `watchdog` observes; when it stops the attempt and the
    /// model still goes on, the run is cancelled with
    /// [`WATCHDOG_STOPPED`](watchdog::WATCHDOG_STOPPED).
    pub(crate) async fn run_watched_agent_step(
        &self,
        request: AgentStepRequest,
        context: &mut StepContext,
        usage: &mut HashMap<AgentId, AgentUsageMetrics>,
        watchdog: Arc<Watchdog>,
    ) -> Result<AgentStepOutput, AgentExecutionError> {
        self.run_agent_step(request, context, usage, Some(watchdog))
            .await
    }

    async fn run_agent_step(
        &self,
        request: AgentStepRequest,
        context: &mut StepContext,
        usage: &mut HashMap<AgentId, AgentUsageMetrics>,
        watchdog: Option<Arc<Watchdog>>,
    ) -> Result<AgentStepOutput, AgentExecutionError> {
        if request.context_mode == rusty_workflow_core::AgentContextMode::SharedSession {
            return Err(AgentExecutionError::new(
                "unsupported_context_mode",
                "shared_session agent steps are not supported yet; use isolated_child",
            ));
        }
        let native_schema = self.native_schema(&request)?;
        let profile = self.step_profile(&request)?;
        let tools = self.step_tools(&request);
        let session = self.open_session(&request, &tools, watchdog.as_ref())?;
        let options = PromptOptions {
            native_schema,
            research: tools.research,
        };
        let mut conversation = Conversation::new(self, &request, context, usage, session, watchdog);
        conversation.research = tools.research;
        conversation.research_tools = tools.research_tools;
        conversation.announce();
        self.start_session(&conversation.session, &request, profile, options)
            .await?;
        conversation.run().await
    }
}

#[async_trait]
impl AgentStepExecutor for IsolatedSessionAgentExecutor {
    async fn execute(
        &self,
        request: AgentStepRequest,
        mut context: StepContext,
    ) -> Result<AgentStepOutput, AgentExecutionError> {
        let mut usage = HashMap::new();
        if request.task_queue.is_some() {
            return self
                .run_planned_task_queue(request, &mut context, &mut usage)
                .await;
        }
        self.run_single_agent_step(request, &mut context, &mut usage)
            .await
    }
}
