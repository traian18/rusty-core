//! Several providers applied one after the other.

use std::sync::Arc;

use async_trait::async_trait;

use rusty_protocol::backend::ExecutionRequest;

use rusty_runtime::traits::Workspace;

use crate::provider::{ContextObservation, ContextProvider};

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

    /// Every provider in the chain gets the request's observation, so a compactor anywhere in it
    /// can report its work.
    async fn assemble_with_observation(
        &self,
        mut request: ExecutionRequest,
        workspace: &dyn Workspace,
        observation: &ContextObservation,
    ) -> ExecutionRequest {
        for provider in &self.providers {
            request = provider
                .assemble_with_observation(request, workspace, observation)
                .await;
        }
        request
    }
}
