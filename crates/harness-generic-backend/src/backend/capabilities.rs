//! Rejects requests the backend cannot serve before any billed provider call.

use harness_model::events::ModelError;
use harness_protocol::backend::{ExecutionRequest, ResponseFormat};
use harness_protocol::messages::ContentBlock;

use super::GenericModelBackend;

impl GenericModelBackend {
    /// Validates the request against this backend's advertised capabilities
    /// *before* any network call is made, so an unsupported request never
    /// causes a billed call to the provider. Returns `Some(error)` to reject
    /// the request, or `None` to proceed.
    pub(super) fn check_capabilities(&self, request: &ExecutionRequest) -> Option<ModelError> {
        self.check_reasoning_capability(request)
            .or_else(|| self.check_image_capability(request))
            .or_else(|| self.check_tool_capability(request))
            .or_else(|| self.check_structured_output_capability(request))
    }

    fn check_reasoning_capability(&self, request: &ExecutionRequest) -> Option<ModelError> {
        let wants_reasoning =
            request.extended_thinking || request.params.reasoning_effort.is_some();
        (wants_reasoning && !self.capabilities.reasoning_stream).then(|| {
            self.unsupported(
                "reasoning",
                "does not support reasoning/extended thinking",
            )
        })
    }

    fn check_image_capability(&self, request: &ExecutionRequest) -> Option<ModelError> {
        let wants_images = request.messages.iter().any(|message| {
            message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Image { .. }))
        });
        (wants_images && !self.capabilities.images)
            .then(|| self.unsupported("images", "does not support image input"))
    }

    fn check_tool_capability(&self, request: &ExecutionRequest) -> Option<ModelError> {
        (!request.tools.is_empty() && !self.capabilities.tool_calls)
            .then(|| self.unsupported("tool_calls", "does not support tool calls"))
    }

    fn check_structured_output_capability(&self, request: &ExecutionRequest) -> Option<ModelError> {
        // A caller asking for JSON and silently receiving prose is the worst
        // outcome here — it fails later, at a parse site far from the cause.
        // `ResponseFormat::Text` is the provider default, so it needs no
        // capability.
        let wants_structured_output = !matches!(
            request.params.response_format,
            None | Some(ResponseFormat::Text)
        );
        (wants_structured_output && !self.capabilities.structured_output).then(|| {
            self.unsupported("structured_output", "cannot constrain its response format")
        })
    }

    fn unsupported(&self, capability: &str, what: &str) -> ModelError {
        ModelError::UnsupportedCapability {
            capability: capability.to_string(),
            detail: format!("{} {what}", self.descriptor.name),
        }
    }
}
