//! One step attempt, run on its own task.

use std::time::Duration;

use harness_core::orchestration::{
    AgentNodeConfig, ApprovalNodeConfig, AttemptDetails, AttemptStatus, InputNodeConfig,
    OrchestrationError, OrchestrationNode, OrchestrationNodeId, OrchestrationNodeKind,
    OrchestrationRunState, OutputNodeConfig, RetryReason, StepRun, StructuredOutputMode,
    SubflowNodeConfig, ToolScope, VerifyNodeConfig,
};
use serde_json::Value;

use super::super::{
    journal,
    steps::{
        execute_approval, execute_input, execute_verify, is_user_change, resolve_binding,
        resolve_inputs, AgentStepRequest, StepContext, SubflowRequest, Validation,
    },
};
use super::OrchestrationRunner;

/// How long a cancelled executor may take to wind down its delegated work
/// (cancel the run, release the session) before its future is dropped.
const CANCEL_GRACE: Duration = Duration::from_secs(5);

pub(super) struct StepExecution {
    pub(super) result: Result<Value, OrchestrationError>,
    pub(super) details: AttemptDetails,
}

/// One attempt, executed on its own task against a snapshot of the state
/// taken at admission (steps run one at a time, so it cannot go stale).
pub(super) struct StepJob {
    pub(super) runner: OrchestrationRunner,
    pub(super) state: OrchestrationRunState,
    pub(super) node: OrchestrationNode,
    pub(super) attempt: u32,
}

impl StepJob {
    pub(super) async fn run(self, context: StepContext) -> StepExecution {
        let cancellation = context.cancellation.clone();
        let mut details = AttemptDetails::default();
        let work = self.execute(context, &mut details);
        // Executors observe the attempt token themselves so they can cancel
        // delegated work cleanly; the grace period bounds a misbehaving one.
        let result = tokio::select! {
            _ = async {
                cancellation.cancelled().await;
                tokio::time::sleep(CANCEL_GRACE).await;
            } => Err(OrchestrationError::new("cancelled", "step cancelled")),
            result = async {
                match self.node.timeout_ms {
                    Some(limit) => tokio::time::timeout(Duration::from_millis(limit), work)
                        .await
                        .unwrap_or_else(|_| Err(self.timeout_error(limit))),
                    None => work.await,
                }
            } => result,
        };
        StepExecution { result, details }
    }

    fn timeout_error(&self, limit: u64) -> OrchestrationError {
        OrchestrationError::retryable(
            "step_timeout",
            format!("step {} timed out after {limit}ms", self.node.id),
            RetryReason::BackendTimeout,
        )
    }

    async fn execute(
        &self,
        context: StepContext,
        details: &mut AttemptDetails,
    ) -> Result<Value, OrchestrationError> {
        match &self.node.kind {
            OrchestrationNodeKind::Input(config) => self.execute_input(config, details),
            OrchestrationNodeKind::Agent(config) => {
                self.execute_agent(config, context, details).await
            }
            OrchestrationNodeKind::Verify(config) => self.execute_verify(config, details).await,
            OrchestrationNodeKind::Subflow(config) => {
                self.execute_subflow(config, context, details).await
            }
            OrchestrationNodeKind::Approval(config) => {
                self.execute_approval(config, context, details).await
            }
            OrchestrationNodeKind::Output(config) => self.execute_output(config),
        }
    }

    fn validation(&self) -> Validation<'_> {
        Validation {
            schemas: self.runner.schemas.as_ref(),
            validator: self.runner.validator.as_ref(),
        }
    }

    fn step(&self) -> &StepRun {
        &self.state.steps[&self.node.id]
    }

    fn execute_input(
        &self,
        config: &InputNodeConfig,
        details: &mut AttemptDetails,
    ) -> Result<Value, OrchestrationError> {
        let input = execute_input(
            &self.state,
            config,
            self.runner.compiled.definition.input_schema.as_ref(),
            &self.validation(),
        )?;
        details.input = self.state.input.clone();
        Ok(input)
    }

    async fn execute_agent(
        &self,
        config: &AgentNodeConfig,
        context: StepContext,
        details: &mut AttemptDetails,
    ) -> Result<Value, OrchestrationError> {
        let validation = self.validation();
        let input = self.agent_input()?;
        details.input = Some(input.clone());
        let schema = self.agent_output_schema(config, &validation)?;
        let tools = self.agent_tools(config)?;
        details.tools = Some(tools.clone());
        let request = self.agent_request(config, input, schema.clone(), tools);
        let output = self.runner.agent.execute(request, context).await?;
        validation.check(
            &schema,
            &output.value,
            "invalid_structured_output",
            Some(RetryReason::InvalidStructuredOutput),
        )?;
        Ok(output.value)
    }

    /// Whether the user asked this step to change its result.
    fn revising(&self) -> bool {
        self.step()
            .feedback
            .iter()
            .any(|item| is_user_change(&item.code))
    }

    fn agent_input(&self) -> Result<Value, OrchestrationError> {
        let mut input = resolve_inputs(&self.state, &self.node.input_bindings)?;
        // Asked to change its result, a step revises what it produced
        // instead of starting over.
        if self.revising() {
            if let (Some(previous), Some(fields)) = (self.previous_success(), input.as_object_mut())
            {
                fields.insert("previous_result".into(), previous);
            }
        }
        Ok(input)
    }

    fn previous_success(&self) -> Option<Value> {
        self.step().attempts.iter().rev().find_map(|attempt| {
            attempt
                .output
                .clone()
                .filter(|_| attempt.status == AttemptStatus::Succeeded)
        })
    }

    fn agent_output_schema(
        &self,
        config: &AgentNodeConfig,
        validation: &Validation<'_>,
    ) -> Result<Value, OrchestrationError> {
        // A task queue builds its own result: the tasks it completed.
        if config.task_queue.is_some() {
            return Ok(serde_json::json!({ "type": "object" }));
        }
        if config.structured_output == StructuredOutputMode::Text {
            return Ok(serde_json::json!({ "type": "string" }));
        }
        let schema_ref = self.node.output_schema.as_ref().ok_or_else(|| {
            OrchestrationError::new("missing_output_schema", "agent node has no output schema")
        })?;
        validation.resolve(schema_ref)
    }

    fn agent_tools(&self, config: &AgentNodeConfig) -> Result<Vec<String>, OrchestrationError> {
        match &config.tools {
            ToolScope::None => Ok(Vec::new()),
            ToolScope::AllowList(tools) => Ok(tools.clone()),
            ToolScope::Inherit => {
                self.runner
                    .available_tools
                    .as_deref()
                    .cloned()
                    .ok_or_else(|| {
                        OrchestrationError::new(
                    "tool_scope_unresolved",
                    "tool scope `inherit` needs the host's tool list (with_available_tools)",
                )
                    })
            }
        }
    }

    fn agent_request(
        &self,
        config: &AgentNodeConfig,
        input: Value,
        output_schema: Value,
        tools: Vec<String>,
    ) -> AgentStepRequest {
        let runner = &self.runner;
        let definition = &runner.compiled.definition;
        let step = self.step();
        // A step asked to change its result already has it as
        // `previous_result`; any other re-run picks up where it was.
        let previous_attempt = (!self.revising())
            .then(|| journal::previous_attempt(step))
            .flatten();
        let journal = journal::journal(definition, &self.state, &self.node, &config.context_access);
        AgentStepRequest {
            task_queue: config.task_queue.clone(),
            checkpoint: step.checkpoint.clone(),
            run_id: self.state.run_id.clone(),
            node_id: self.node.id.clone(),
            attempt: self.attempt,
            instructions: config.instructions.clone(),
            input,
            output_schema,
            tools,
            model: config.model.clone(),
            structured_output: config.structured_output,
            context_mode: config.context_mode,
            profile: config.profile.clone(),
            feedback: step.feedback.clone(),
            plan_revisable: runner.compiled.retry_spans.contains_key(&self.node.id)
                && self.node.task_plan_source().is_some(),
            subflow_depth: runner.depth + 1,
            run_options: self.state.options.clone(),
            previous_attempt,
            journal,
            research: definition.policies.research.clone().unwrap_or_default(),
        }
    }

    async fn execute_verify(
        &self,
        config: &VerifyNodeConfig,
        details: &mut AttemptDetails,
    ) -> Result<Value, OrchestrationError> {
        let validation = self.validation();
        let compiled = &self.runner.compiled;
        let producers = |node_id: &OrchestrationNodeId| {
            compiled
                .node(node_id)
                .and_then(|node| node.output_schema.as_ref())
                .and_then(|reference| validation.resolve(reference).ok())
        };
        let (result, evidence) = execute_verify(
            &self.state,
            &self.node,
            config,
            &producers,
            &validation,
            self.runner.artifacts.as_ref(),
        )
        .await;
        details.evidence = evidence;
        result
    }

    async fn execute_subflow(
        &self,
        config: &SubflowNodeConfig,
        mut context: StepContext,
        details: &mut AttemptDetails,
    ) -> Result<Value, OrchestrationError> {
        let input = resolve_inputs(&self.state, &self.node.input_bindings)?;
        details.input = Some(input.clone());
        let executor = self.runner.subflows.as_ref().ok_or_else(|| {
            OrchestrationError::new(
                "subflows_unavailable",
                "this host cannot run flows inside flows",
            )
        })?;
        let request = SubflowRequest {
            run_id: self.state.run_id.clone(),
            node_id: self.node.id.clone(),
            attempt: self.attempt,
            target: config.target.clone(),
            input,
            depth: self.runner.depth + 1,
            options: self.state.options.clone(),
        };
        Ok(executor.execute(request, &mut context).await?)
    }

    async fn execute_approval(
        &self,
        config: &ApprovalNodeConfig,
        mut context: StepContext,
        details: &mut AttemptDetails,
    ) -> Result<Value, OrchestrationError> {
        details.input = resolve_binding(&self.state, &config.subject).ok();
        execute_approval(&self.state, &self.node, config, self.attempt, &mut context).await
    }

    fn execute_output(&self, config: &OutputNodeConfig) -> Result<Value, OrchestrationError> {
        let definition = &self.runner.compiled.definition;
        let value = resolve_binding(&self.state, &config.source)?;
        // The definition's output contract is the final public
        // contract; `strict = false` on both skips validation.
        if config.strict || definition.output_contract.strict {
            let validation = self.validation();
            let schema = validation.resolve(&definition.output_contract.schema)?;
            validation.check(&schema, &value, "invalid_final_output", None)?;
        }
        Ok(value)
    }
}
