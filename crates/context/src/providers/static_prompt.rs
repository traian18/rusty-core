//! A fixed instruction string prepended to every request.

use async_trait::async_trait;

use rusty_protocol::backend::ExecutionRequest;

use rusty_runtime::traits::Workspace;

use crate::provider::ContextProvider;

/// Prepends a fixed instruction string to the system prompt — the
/// IDE-integration equivalent of a `CLAUDE.md`: a static project/product
/// instruction set that every request should carry.
pub struct StaticSystemPromptProvider {
    instructions: String,
}

impl StaticSystemPromptProvider {
    pub fn new(instructions: impl Into<String>) -> Self {
        Self {
            instructions: instructions.into(),
        }
    }
}

#[async_trait]
impl ContextProvider for StaticSystemPromptProvider {
    async fn assemble(
        &self,
        mut request: ExecutionRequest,
        _workspace: &dyn Workspace,
    ) -> ExecutionRequest {
        request.system_prompt = if request.system_prompt.is_empty() {
            self.instructions.clone()
        } else {
            format!("{}\n\n{}", self.instructions, request.system_prompt)
        };
        request
    }
}
