//! The fresh session a step runs in: its scoped tools, its runtime, and how it is configured.
//!
//! The session gets a fresh transcript, agent id, run id, cancellation tree, scoped backend
//! request view and scoped executor registry. From the parent it inherits only host dependencies
//! (backend, workspace, integrations), the parent's execution parameters, and the permission
//! policy of each tool it may use.

use std::sync::Arc;

use async_trait::async_trait;
use rusty_agent_core::behavior::default_profile;
use rusty_protocol::{
    backend::ResponseFormat,
    commands::UserInput,
    events::AgentEventEnvelope,
    ids::{AgentId, SessionId},
};
use rusty_runtime::{
    scoped_tools::{ScopedExecutionBackend, ScopedToolRegistry},
    session_runtime::{SessionCommand, SessionRuntime},
    traits::{EventSink, ToolRegistry},
};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use super::profile::{document, StepProfile};
use super::prompt::{self, PromptOptions};
use super::research::{ResearchCall, ResearchTool, WithResearch};
use super::toolset::StepTools;
use super::watchdog::{Watchdog, WatchedRegistry};
use super::IsolatedSessionAgentExecutor;
use crate::orchestration::{AgentExecutionError, AgentStepRequest};

/// The isolated session's events are observed through its event bus and republished by the
/// orchestration runner with step correlation, so the session itself has no external sink.
struct NullEventSink;

#[async_trait]
impl EventSink for NullEventSink {
    fn send(&self, _envelope: AgentEventEnvelope) {}
}

/// Shuts the session down and forgets it however the attempt ends. Until then the parent can reach
/// it, so the host may change the step's model while it runs.
struct Shutdown {
    session: Arc<SessionRuntime>,
    parent: Arc<SessionRuntime>,
}

impl Drop for Shutdown {
    fn drop(&mut self) {
        self.parent.unregister_delegated(self.session.session_id);
        self.session.shutdown();
    }
}

/// A running isolated session and the channels the step's event loop listens on.
pub(super) struct IsolatedSession {
    pub runtime: Arc<SessionRuntime>,
    pub events: broadcast::Receiver<AgentEventEnvelope>,
    pub research_queue: mpsc::UnboundedReceiver<ResearchCall>,
    pub hard_stop: CancellationToken,
    pub root_agent_id: AgentId,
    _shutdown: Shutdown,
}

fn failed(
    code: &'static str,
) -> impl Fn(rusty_runtime::session_runtime::SessionError) -> AgentExecutionError {
    move |error| AgentExecutionError::new(code, error.to_string())
}

impl IsolatedSessionAgentExecutor {
    /// Starts an idle session with only the step's tools.
    pub(super) fn open_session(
        &self,
        request: &AgentStepRequest,
        tools: &StepTools,
        watchdog: Option<&Arc<Watchdog>>,
    ) -> Result<IsolatedSession, AgentExecutionError> {
        let (research_calls, research_queue) = mpsc::unbounded_channel::<ResearchCall>();
        let registry = self.step_registry(request, tools, research_calls, watchdog);
        let backend = Arc::new(ScopedExecutionBackend::new(
            self.parent.default_backend.clone(),
            tools.names.clone(),
        ));
        let toolset = self.scoped_toolset(registry.as_ref());
        let runtime = Arc::new(SessionRuntime::new_with_toolset(
            SessionId::new(),
            backend,
            registry,
            self.parent.workspace.clone(),
            Arc::new(NullEventSink),
            toolset,
        ));
        self.parent.register_delegated(&runtime);
        let shutdown = Shutdown {
            session: runtime.clone(),
            parent: self.parent.clone(),
        };
        runtime
            .integrations
            .extend_from(&self.parent.integrations)
            .map_err(|error| {
                AgentExecutionError::new("integration_scope_failed", error.to_string())
            })?;
        Ok(IsolatedSession {
            events: runtime.event_bus.subscribe(),
            root_agent_id: runtime.state_snapshot().root_agent_id,
            hard_stop: watchdog
                .map(|watchdog| watchdog.hard_stop())
                .unwrap_or_default(),
            research_queue,
            runtime,
            _shutdown: shutdown,
        })
    }

    /// The step's tools, with research offered and calls watched where the step asks for it.
    fn step_registry(
        &self,
        request: &AgentStepRequest,
        tools: &StepTools,
        research_calls: mpsc::UnboundedSender<ResearchCall>,
        watchdog: Option<&Arc<Watchdog>>,
    ) -> Arc<dyn ToolRegistry> {
        let scoped: Arc<dyn ToolRegistry> = Arc::new(ScopedToolRegistry::new(
            self.parent.tool_registry.clone(),
            tools.names.clone(),
        ));
        let registry: Arc<dyn ToolRegistry> = if tools.research {
            Arc::new(WithResearch {
                inner: scoped,
                research: Arc::new(ResearchTool {
                    calls: research_calls,
                    max: request.research.max_per_attempt,
                }),
            })
        } else {
            scoped
        };
        match watchdog {
            Some(watchdog) => Arc::new(WatchedRegistry {
                inner: registry,
                watchdog: watchdog.clone(),
            }),
            None => registry,
        }
    }

    /// Applies the step's model, profile and response format, then sends the prompt that starts
    /// the run.
    pub(super) async fn start_session(
        &self,
        session: &IsolatedSession,
        request: &AgentStepRequest,
        profile: StepProfile,
        options: PromptOptions,
    ) -> Result<(), AgentExecutionError> {
        self.configure_execution(session, request, options.native_schema)
            .await?;
        self.configure_behavior(session, profile).await?;
        let text = prompt::build(request, &options)?;
        session
            .runtime
            .send_command(SessionCommand::Prompt(UserInput {
                text,
                attachments: vec![],
            }))
            .await
            .map_err(failed("session_start_failed"))
    }

    async fn configure_execution(
        &self,
        session: &IsolatedSession,
        request: &AgentStepRequest,
        native_schema: bool,
    ) -> Result<(), AgentExecutionError> {
        let mut params = self.parent.root_execution_params();
        if request.model.is_some() {
            params.model = request.model.clone();
        }
        params.response_format = native_schema.then(|| ResponseFormat::JsonSchema {
            name: format!("{}_output", request.node_id.as_str()),
            schema: request.output_schema.clone(),
            strict: true,
        });
        session
            .runtime
            .send_command(SessionCommand::ConfigureExecution(params))
            .await
            .map_err(failed("session_configuration_failed"))
    }

    async fn configure_behavior(
        &self,
        session: &IsolatedSession,
        (profile, library): StepProfile,
    ) -> Result<(), AgentExecutionError> {
        if profile.content_hash == default_profile().content_hash && library.is_empty() {
            return Ok(());
        }
        // The session is idle, so the profile applies to the first run.
        session
            .runtime
            .send_command(SessionCommand::SetBehaviorBundle {
                profile: document(&profile.profile),
                library,
                // Steps run with the parent session's command trust.
                allow_commands: self.parent.root_behavior().commands_trusted,
            })
            .await
            .map_err(failed("session_configuration_failed"))
    }
}
