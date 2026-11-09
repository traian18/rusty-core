//! Following a step's session until it answers.
//!
//! The session talks through its event bus; the step also listens for cancellation, the watchdog's
//! hard stop, permission decisions from the host, and research questions. Each wake-up is one small
//! handler, and the loop ends with the step's output or an error.

mod root_events;

use std::collections::HashMap;
use std::sync::Arc;

use rusty_protocol::{
    events::AgentEventEnvelope,
    ids::{AgentId, PermissionId},
    usage::AgentUsageMetrics,
};
use rusty_workflow_core::DelegatedRunRef;
use tokio::sync::broadcast;

use super::answer_text::AnswerText;
use super::research::ResearchCall;
use super::session::IsolatedSession;
use super::usage::usage_summary;
use super::watchdog::{Watchdog, WATCHDOG_STOPPED};
use super::IsolatedSessionAgentExecutor;
use crate::orchestration::{
    AgentExecutionError, AgentStepOutput, AgentStepRequest, PermissionResolution, StepContext,
    StepSignal,
};
use rusty_protocol::events::AgentEvent;

/// What woke the loop. One lives only until it is matched, so its size does not matter.
#[allow(clippy::large_enum_variant)]
enum Wake {
    Cancelled,
    HardStop,
    Permission(Option<PermissionResolution>),
    Research(Option<ResearchCall>),
    Event(Result<AgentEventEnvelope, broadcast::error::RecvError>),
}

pub(super) struct Conversation<'a> {
    pub executor: &'a IsolatedSessionAgentExecutor,
    pub request: &'a AgentStepRequest,
    pub context: &'a mut StepContext,
    pub usage: &'a mut HashMap<AgentId, AgentUsageMetrics>,
    pub session: IsolatedSession,
    pub watchdog: Option<Arc<Watchdog>>,
    pub research: bool,
    pub research_tools: Vec<String>,
    pub delegated: DelegatedRunRef,
    permissions: HashMap<String, PermissionId>,
    permissions_open: bool,
    answer: AnswerText,
    /// Checks still failing when the completion gate gave up.
    unpassed_gate: Option<Vec<String>>,
    research_asked: u32,
}

impl<'a> Conversation<'a> {
    pub(super) fn new(
        executor: &'a IsolatedSessionAgentExecutor,
        request: &'a AgentStepRequest,
        context: &'a mut StepContext,
        usage: &'a mut HashMap<AgentId, AgentUsageMetrics>,
        session: IsolatedSession,
        watchdog: Option<Arc<Watchdog>>,
    ) -> Self {
        let delegated = delegated_run(&session);
        Self {
            executor,
            request,
            context,
            usage,
            session,
            watchdog,
            research: false,
            research_tools: Vec::new(),
            delegated,
            permissions: HashMap::new(),
            permissions_open: true,
            answer: AnswerText::default(),
            unpassed_gate: None,
            research_asked: 0,
        }
    }

    /// Tells the host which session runs the step.
    pub(super) fn announce(&mut self) {
        self.context
            .signal(StepSignal::Delegated(self.delegated.clone()));
    }

    pub(super) async fn run(mut self) -> Result<AgentStepOutput, AgentExecutionError> {
        loop {
            let envelope = match self.wake().await {
                Wake::Cancelled => return Err(self.cancelled().await),
                Wake::HardStop => return Err(self.hard_stopped().await),
                Wake::Permission(resolution) => {
                    self.deliver_permission(resolution).await?;
                    continue;
                }
                Wake::Research(call) => {
                    self.answer_research(call).await;
                    continue;
                }
                Wake::Event(Ok(envelope)) => envelope,
                Wake::Event(Err(error)) => return Err(self.events_ended(error).await),
            };
            self.context
                .signal(StepSignal::Agent(Box::new(envelope.clone())));
            self.track_usage(&envelope);
            if envelope.agent_id != self.session.root_agent_id {
                continue;
            }
            if let Some(output) = self.on_root_event(envelope.event).await? {
                return Ok(output);
            }
        }
    }

    async fn wake(&mut self) -> Wake {
        let permissions_open = self.permissions_open;
        let research = self.research;
        tokio::select! {
            _ = self.context.cancellation.cancelled() => Wake::Cancelled,
            _ = self.session.hard_stop.cancelled() => Wake::HardStop,
            resolution = self.context.permissions.recv(), if permissions_open => {
                Wake::Permission(resolution)
            }
            call = self.session.research_queue.recv(), if research => Wake::Research(call),
            event = self.session.events.recv() => Wake::Event(event),
        }
    }

    async fn cancelled(&self) -> AgentExecutionError {
        let _ = self.session.runtime.cancel_run().await;
        AgentExecutionError::new("cancelled", "agent attempt cancelled")
    }

    async fn hard_stopped(&self) -> AgentExecutionError {
        let _ = self.session.runtime.cancel_run().await;
        let reason = self
            .watchdog
            .as_ref()
            .and_then(|watchdog| watchdog.stopped())
            .unwrap_or_default();
        AgentExecutionError::new(WATCHDOG_STOPPED, reason)
    }

    async fn events_ended(&self, error: broadcast::error::RecvError) -> AgentExecutionError {
        match error {
            broadcast::error::RecvError::Lagged(count) => {
                let _ = self.session.runtime.cancel_run().await;
                AgentExecutionError::new(
                    "agent_event_lagged",
                    format!(
                        "isolated session dropped {count} events; the result cannot be trusted"
                    ),
                )
            }
            broadcast::error::RecvError::Closed => AgentExecutionError::new(
                "agent_event_stream_closed",
                "isolated session event stream closed before completion",
            ),
        }
    }

    /// Relays the host's decision to the session that asked for the permission.
    async fn deliver_permission(
        &mut self,
        resolution: Option<PermissionResolution>,
    ) -> Result<(), AgentExecutionError> {
        let Some(resolution) = resolution else {
            self.permissions_open = false;
            return Ok(());
        };
        let Some(id) = self.permissions.remove(&resolution.permission_id) else {
            return Ok(());
        };
        self.session
            .runtime
            .resolve_permission(id, resolution.decision)
            .await
            .map_err(|error| {
                AgentExecutionError::new("permission_delivery_failed", error.to_string())
            })
    }

    /// Runs the research the step asked for, within the per-attempt limit.
    async fn answer_research(&mut self, call: Option<ResearchCall>) {
        let Some(call) = call else { return };
        let limit = self.request.research.max_per_attempt;
        let answer = if self.research_asked >= limit {
            Err(format!(
                "research limit reached ({limit} per attempt): go on with what you have"
            ))
        } else {
            self.research_asked += 1;
            self.executor
                .research(
                    self.request,
                    &call,
                    self.research_asked,
                    &self.research_tools,
                    self.context,
                )
                .await
        };
        let _ = call.reply.send(answer);
    }

    fn track_usage(&mut self, envelope: &AgentEventEnvelope) {
        if let AgentEvent::UsageUpdated { usage: snapshot } = &envelope.event {
            self.usage
                .insert(envelope.agent_id, snapshot.metrics.clone());
            self.context
                .signal(StepSignal::Usage(usage_summary(self.usage)));
        }
    }
}

fn delegated_run(session: &IsolatedSession) -> DelegatedRunRef {
    DelegatedRunRef {
        session_id: Some(session.runtime.session_id.to_string()),
        agent_id: Some(session.root_agent_id.to_string()),
        agent_run_id: None,
    }
}
