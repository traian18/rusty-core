//! The agent check: an isolated verifier session that may read with tools before it answers.
//!
//! The verifier runs in a fresh session with only the tools the check names. It cannot ask for
//! approval, so each of those tools must be allowed outright in the session being checked.

use std::sync::Arc;

use rusty_protocol::backend::{ExecutionParams, ResponseFormat};
use rusty_protocol::commands::{UserInput, VerdictOutcome};
use rusty_protocol::effects::CompletionEvaluationRequest;
use rusty_protocol::events::{AgentEvent, AgentEventEnvelope, AgentOutcome};
use rusty_protocol::ids::{SessionId, ToolId};
use rusty_protocol::messages::AgentMessage;
use rusty_protocol::tools::{
    AgentToolset, PermissionMode, ToolCapability, ToolDescriptor, ToolPolicy,
};
use rusty_protocol::usage::UsageRecord;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::text::{render_transcript, truncate};
use super::usage::verifier_usage;
use super::verdict::{parse_verdict, verdict_schema, VERIFIER_PROMPT};
use super::{GateContext, MAX_FEEDBACK_CHARS};
use crate::scoped_tools::{ScopedExecutionBackend, ScopedToolRegistry};
use crate::session_runtime::{SessionCommand, SessionRuntime};
use crate::traits::{EventSink, ToolRegistry};

pub(super) struct VerifierSpec<'a> {
    pub instructions: &'a str,
    pub tools: Vec<String>,
    pub model: Option<String>,
    pub max_turns: u32,
}

struct NullSink;

impl EventSink for NullSink {
    fn send(&self, _envelope: AgentEventEnvelope) {}
}

/// Runs a verifier agent and returns its verdict with the usage of the requests it made.
pub(super) async fn run_verifier_agent(
    context: &GateContext,
    request: &CompletionEvaluationRequest,
    spec: VerifierSpec<'_>,
    transcript: &[AgentMessage],
    cancel: CancellationToken,
) -> (VerdictOutcome, Vec<UsageRecord>) {
    let not_allowed = spec.tools.iter().find(|tool| {
        !matches!(
            context.tool_permissions.get(*tool),
            Some(PermissionMode::Allow)
        )
    });
    if let Some(tool) = not_allowed {
        let message =
            format!("verifier tool {tool} is not allowed without approval in this session");
        return (VerdictOutcome::Error { message }, Vec::new());
    }
    let runtime = open_session(context, &spec.tools);
    let outcome = drive_verifier(context, request, &spec, transcript, &runtime, &cancel).await;
    let root = runtime.state_snapshot().root_agent_id;
    let live = runtime.agent_live_state(root);
    runtime.shutdown();
    (outcome, verifier_usage(&live))
}

/// An idle session whose only tools are the verifier's.
fn open_session(context: &GateContext, tools: &[String]) -> SessionRuntime {
    let registry: Arc<dyn ToolRegistry> = Arc::new(ScopedToolRegistry::new(
        context.tool_registry.clone(),
        tools.to_vec(),
    ));
    let backend = Arc::new(ScopedExecutionBackend::new(
        context.backend.clone(),
        tools.to_vec(),
    ));
    let toolset = allow_all(registry.as_ref());
    SessionRuntime::new_with_toolset(
        SessionId::new(),
        backend,
        registry,
        context.workspace.clone(),
        Arc::new(NullSink),
        toolset,
    )
}

fn allow_all(registry: &dyn ToolRegistry) -> AgentToolset {
    let tools = registry
        .descriptors()
        .into_iter()
        .map(|descriptor| {
            let id = ToolId::new();
            let capability = ToolCapability {
                descriptor: ToolDescriptor {
                    id,
                    name: descriptor.id.to_string(),
                    description: descriptor.description,
                    input_schema: descriptor.input_schema,
                },
                policy: ToolPolicy {
                    permission: PermissionMode::Allow,
                    enabled: true,
                },
                delegatable: false,
            };
            (id, capability)
        })
        .collect();
    AgentToolset { tools }
}

async fn drive_verifier(
    context: &GateContext,
    request: &CompletionEvaluationRequest,
    spec: &VerifierSpec<'_>,
    transcript: &[AgentMessage],
    runtime: &SessionRuntime,
    cancel: &CancellationToken,
) -> VerdictOutcome {
    let mut events = runtime.event_bus.subscribe();
    let commands = [
        SessionCommand::ConfigureExecution(execution_params(context, spec)),
        SessionCommand::SetBehaviorProfile(behavior_profile(spec)),
        SessionCommand::Prompt(UserInput {
            text: prompt(request, transcript),
            attachments: vec![],
        }),
    ];
    for command in commands {
        if let Err(error) = runtime.send_command(command).await {
            let message = format!("verifier session failed to start: {error}");
            return VerdictOutcome::Error { message };
        }
    }
    follow(runtime, &mut events, cancel).await
}

fn execution_params(context: &GateContext, spec: &VerifierSpec<'_>) -> ExecutionParams {
    let mut params = ExecutionParams {
        model: spec.model.clone().or_else(|| context.params.model.clone()),
        ..Default::default()
    };
    if context.backend.capabilities().structured_output {
        params.response_format = Some(ResponseFormat::JsonSchema {
            name: "completion_verdict".into(),
            schema: verdict_schema(),
            strict: true,
        });
    }
    params
}

fn behavior_profile(spec: &VerifierSpec<'_>) -> Value {
    json!({
        "schema_version": 1, "id": "gate-verifier", "revision": 1, "name": "Verifier",
        "instructions": {
            "mode": "replace",
            "text": format!("{VERIFIER_PROMPT}\n\nYou may use your tools to check the work before answering.\n\nCriteria:\n{}", spec.instructions)
        },
        "tools": { "type": "allow_list", "tools": spec.tools },
        "limits": {
            "max_turns": spec.max_turns,
            "final_turn_prompt": "Give your verdict now as JSON: {\"passed\": boolean, \"feedback\": string}."
        }
    })
}

fn prompt(request: &CompletionEvaluationRequest, transcript: &[AgentMessage]) -> String {
    format!(
        "<transcript>\n{}</transcript>\n\n<proposed_answer>\n{}\n</proposed_answer>\n\nInvestigate as needed, then reply with JSON only.",
        render_transcript(transcript),
        truncate(&request.final_response, MAX_FEEDBACK_CHARS * 4),
    )
}

/// Reads the verifier's events until it completes; its verdict is its last message with text.
async fn follow(
    runtime: &SessionRuntime,
    events: &mut broadcast::Receiver<AgentEventEnvelope>,
    cancel: &CancellationToken,
) -> VerdictOutcome {
    let root = runtime.state_snapshot().root_agent_id;
    let mut reply = Reply::default();
    loop {
        let envelope = tokio::select! {
            _ = cancel.cancelled() => {
                let _ = runtime.cancel_run().await;
                return VerdictOutcome::Error { message: "evaluation cancelled".into() };
            }
            event = events.recv() => match event {
                Ok(envelope) => envelope,
                Err(error) => {
                    let message = format!("verifier event stream failed: {error}");
                    return VerdictOutcome::Error { message };
                }
            },
        };
        if envelope.agent_id != root {
            continue;
        }
        if let Some(outcome) = reply.on_event(envelope.event) {
            return outcome;
        }
    }
}

/// The verifier's messages so far: the one being written and the last one that had text.
#[derive(Default)]
struct Reply {
    current: String,
    last: Option<String>,
}

impl Reply {
    /// The verdict, once the event ends the run.
    fn on_event(&mut self, event: AgentEvent) -> Option<VerdictOutcome> {
        match event {
            AgentEvent::AssistantMessageStarted { .. } => self.current.clear(),
            AgentEvent::AssistantTextDelta { delta, .. } => self.current.push_str(&delta),
            AgentEvent::AssistantMessageCompleted { .. } => {
                if !self.current.trim().is_empty() {
                    self.last = Some(std::mem::take(&mut self.current));
                }
            }
            AgentEvent::Failed { error } => {
                let message = format!("verifier failed: {}", error.message);
                return Some(VerdictOutcome::Error { message });
            }
            AgentEvent::Completed { outcome } => return Some(self.verdict(outcome)),
            _ => {}
        }
        None
    }

    fn verdict(&mut self, outcome: AgentOutcome) -> VerdictOutcome {
        if outcome != AgentOutcome::Success {
            return VerdictOutcome::Error {
                message: format!("verifier finished with {outcome:?}"),
            };
        }
        let text = self
            .last
            .take()
            .unwrap_or_else(|| std::mem::take(&mut self.current));
        match parse_verdict(&text) {
            Ok((true, _)) => VerdictOutcome::Pass,
            Ok((false, feedback)) => VerdictOutcome::Fail {
                feedback: failure_feedback(&feedback),
            },
            Err(message) => VerdictOutcome::Error { message },
        }
    }
}

fn failure_feedback(feedback: &str) -> String {
    if feedback.trim().is_empty() {
        "The verifier rejected the answer without details.".into()
    } else {
        truncate(feedback, MAX_FEEDBACK_CHARS)
    }
}
