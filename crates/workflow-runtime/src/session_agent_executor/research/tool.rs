//! `request_research`, the tool a step calls to ask a research question.

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{ResearchCall, RESEARCH_TOOL};

/// `request_research`: hands the question to the step's own event loop,
/// which runs the research step (it holds what a child run needs: the
/// step's context, its permissions and questions to the user).
pub(in crate::session_agent_executor) struct ResearchTool {
    pub(in crate::session_agent_executor) calls: mpsc::UnboundedSender<ResearchCall>,
    pub(in crate::session_agent_executor) max: u32,
}

#[derive(serde::Deserialize)]
struct ResearchArgs {
    question: String,
    #[serde(default)]
    why: Option<String>,
}

#[async_trait]
impl rusty_tool_api::ToolExecutor for ResearchTool {
    fn descriptor(&self) -> rusty_tool_api::ToolDescriptor {
        rusty_tool_api::ToolDescriptor {
            id: rusty_tool_api::ToolId::new(RESEARCH_TOOL),
            name: "Request research".into(),
            description: format!(
                "Ask a read-only research step a question your input does not answer: how something in the project works, where something is defined or used, what an outside library or API expects. It runs right away with read tools only and returns its findings with file:line or URL evidence. Use it when finding the answer yourself would take many reads; do not use it for something one read_file answers. At most {} per attempt.",
                self.max
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "question": { "type": "string", "description": "One specific question." },
                    "why": { "type": "string", "description": "What you need the answer for, so the research stays on point." }
                },
                "required": ["question"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(
        &self,
        input: rusty_tool_api::ToolInput,
        cancel: CancellationToken,
    ) -> Result<rusty_tool_api::ToolResult, rusty_tool_api::ToolError> {
        let result = |output: Value, is_error: bool| rusty_tool_api::ToolResult {
            call_id: RESEARCH_TOOL.into(),
            output,
            is_error,
        };
        let Ok(args) = input.parse::<ResearchArgs>() else {
            return Ok(result(
                json!({ "error": "request_research needs a `question`" }),
                true,
            ));
        };
        if args.question.trim().is_empty() {
            return Ok(result(json!({ "error": "the question is empty" }), true));
        }
        let (reply, answer) = oneshot::channel();
        // Ends the research if this call is cancelled or times out first.
        let guard = cancel.child_token();
        let _ends_research = guard.clone().drop_guard();
        self.calls
            .send(ResearchCall {
                question: args.question,
                why: args.why.filter(|why| !why.trim().is_empty()),
                cancel: guard,
                reply,
            })
            .map_err(|_| rusty_tool_api::ToolError::ExecutionFailed)?;
        let answer = tokio::select! {
            answer = answer => answer.map_err(|_| rusty_tool_api::ToolError::ExecutionFailed)?,
            _ = cancel.cancelled() => return Err(rusty_tool_api::ToolError::Timeout),
        };
        Ok(match answer {
            Ok(findings) => result(json!({ "findings": findings }), false),
            Err(error) => result(json!({ "error": error }), true),
        })
    }
}
