//! Several providers applied one after the other.

use std::sync::Arc;

use async_trait::async_trait;

use rusty_protocol::backend::ExecutionRequest;

use rusty_runtime::traits::Workspace;

use crate::provider::ContextProvider;

/// Prepends a fixed instruction string to the system prompt — the

/// Composes a sequence of providers, running each in order — e.g.
/// `WorkspaceInfoProvider` then `TruncatingCompactionProvider`.
pub struct ChainedContextProvider {
    providers: Vec<Arc<dyn ContextProvider>>,
}

impl ChainedContextProvider {
    pub fn new(providers: Vec<Arc<dyn ContextProvider>>) -> Self {
        Self { providers }
    }
}

#[async_trait]
impl ContextProvider for ChainedContextProvider {
    async fn assemble(
        &self,
        mut request: ExecutionRequest,
        workspace: &dyn Workspace,
    ) -> ExecutionRequest {
        for provider in &self.providers {
            request = provider.assemble(request, workspace).await;
        }
        request
    }
}
