//! Running one research step for a `request_research` call.

use serde_json::{json, Value};

use super::{ResearchCall, RESEARCH_INSTRUCTIONS};
use crate::markdown_reply;
use crate::orchestration::{AgentStepRequest, StepContext, SubflowRequest};
use crate::session_agent_executor::IsolatedSessionAgentExecutor;
use rusty_workflow_core::{SubflowTarget, ToolScope};

impl IsolatedSessionAgentExecutor {
    /// Runs one research step for `call` and returns its findings. It runs
    /// as a child flow of the asking step, under a cancellation that ends
    /// with the step or with the tool call, whichever comes first.
    pub(in crate::session_agent_executor) async fn research(
        &self,
        request: &AgentStepRequest,
        call: &ResearchCall,
        number: u32,
        tools: &[String],
        context: &mut StepContext,
    ) -> Result<String, String> {
        let subflows = self
            .subflows
            .as_ref()
            .ok_or("this host cannot run research")?;
        let mut input = serde_json::Map::new();
        input.insert("question".into(), json!(call.question));
        if let Some(why) = &call.why {
            input.insert("why".into(), json!(why));
        }
        if let Some(asked) = request
            .input
            .get("request")
            .filter(|value| !value.is_null())
        {
            input.insert("request".into(), asked.clone());
        }
        let step = context.cancellation.clone();
        let scoped = step.child_token();
        let ends = call.cancel.clone();
        let stop = scoped.clone();
        let watcher = tokio::spawn(async move {
            tokio::select! {
                _ = ends.cancelled() => stop.cancel(),
                _ = stop.cancelled() => {}
            }
        });
        context.cancellation = scoped.clone();
        let result = subflows
            .execute(
                SubflowRequest {
                    run_id: request.run_id.clone(),
                    node_id: rusty_workflow_core::OrchestrationNodeId::new(format!(
                        "{}.research{number}",
                        request.node_id
                    )),
                    attempt: request.attempt,
                    target: SubflowTarget::Step {
                        instructions: RESEARCH_INSTRUCTIONS.into(),
                        profile: request.research.profile.clone(),
                        tools: ToolScope::AllowList(tools.to_vec()),
                        model: None,
                    },
                    input: Value::Object(input),
                    depth: request.subflow_depth,
                    options: request.run_options.clone(),
                },
                context,
            )
            .await;
        context.cancellation = step;
        scoped.cancel();
        watcher.abort();
        match result {
            Ok(Value::String(findings)) => Ok(findings),
            Ok(other) => Ok(markdown_reply::render_input(&other)),
            Err(error) => Err(format!("research failed: {}", error.message)),
        }
    }
}
