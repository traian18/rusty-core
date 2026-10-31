//! Starting a model request.

use rusty_agent_core::transcript::validate_transcript;
use rusty_protocol::backend::ExecutionRequest;
use rusty_protocol::events::AgentEvent;

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
        let token = self.cancel.child_token();
        self.backend_tokens.insert(request.run_id, token.clone());
        BackendRun {
            backend: self.backend.clone(),
            scheduler: self.scheduler.clone(),
            commands: self.task.commands_tx.clone(),
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
