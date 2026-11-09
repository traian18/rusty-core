//! Starting a model request.

use rusty_agent_core::transcript::validate_transcript;
use rusty_protocol::backend::ExecutionRequest;
use rusty_protocol::events::AgentEvent;
use tokio_util::sync::CancellationToken;

use super::backend_run::{invalid_request, BackendRun};
use super::mailbox::deliver;

use super::AgentRunner;

impl AgentRunner {
    /// Streams a backend request into the agent's mailbox.
    ///
    /// Repair the outgoing history, then validate it before dispatch. The
    /// recorded transcript stays untouched. The request then runs off the
    /// loop (see [`BackendRun`]).
    pub(super) async fn execute_backend(&mut self, mut request: ExecutionRequest) {
        request.messages = rusty_protocol::messages::repair_tool_history(request.messages);
        if let Err(message) = self.check_backend_request(&request) {
            let refused = invalid_request(request.run_id, request.request_id, message);
            deliver(&self.task.commands_tx, refused).await;
            return;
        }

        let traced = request.params.provider_options["rusty"]["trace_model_requests"] == true;
        if traced {
            let prepared = AgentEvent::ModelRequestPrepared {
                request: Box::new(request.clone()),
            };
            if !self.emit(prepared).await {
                return;
            }
        }
        let changed = self.prompt_prefix.observe(&request);
        if !changed.is_empty() {
            let event = AgentEvent::PromptPrefixChanged {
                run_id: request.run_id,
                request_id: request.request_id,
                changed,
            };
            if !self.emit(event).await {
                return;
            }
        }
        let token = CancellationToken::new();
        let request_start_order = self.request_start_order;
        let Some(next_order) = request_start_order.checked_add(1) else {
            deliver(
                &self.task.commands_tx,
                invalid_request(
                    request.run_id,
                    request.request_id,
                    "request start order exhausted".into(),
                ),
            )
            .await;
            return;
        };
        self.request_start_order = next_order;
        let binding = super::backend_run::BindingGuard::register(
            self.observation_bindings.clone(),
            crate::traits::RequestObservationBinding {
                session_id: self.agent.session_id,
                agent_id: self.agent.id,
                parent_agent_id: self.agent.parent_id,
                run_id: request.run_id,
                request_id: request.request_id,
                request_start_order,
                publisher: super::observation_publisher::publisher(
                    self.task.observation_tx.clone(),
                ),
                session_cancellation: self.cancel.clone(),
            },
        );
        if !self
            .emit(AgentEvent::ContextRequestStarted {
                request_id: request.request_id,
                request_start_order,
                model: request.params.model.clone(),
            })
            .await
        {
            return;
        }
        self.backend_tokens.insert(request.run_id, token.clone());
        BackendRun {
            backend: self.backend.clone(),
            scheduler: self.scheduler.clone(),
            commands: self.task.commands_tx.clone(),
            binding,
            runner_cancel: self.cancel.clone(),
            token,
        }
        .start(request);
    }

    /// Why `request` must not reach the backend, if it must not.
    pub(super) fn check_backend_request(&self, request: &ExecutionRequest) -> Result<(), String> {
        // Covers direct runtime construction, restore and child backends too.
        if self.backend.capabilities().backend_managed_tools {
            return Err("Integration must return tool calls to the harness; backend-managed execution is disabled".into());
        }
        validate_transcript(&request.messages).map_err(|error| {
            tracing::error!(
                ?error,
                "refusing to dispatch backend request with invalid transcript"
            );
            format!("invalid transcript: {error}")
        })
    }
}
