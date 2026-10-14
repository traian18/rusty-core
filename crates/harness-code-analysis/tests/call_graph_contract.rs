use std::sync::Arc;

use async_trait::async_trait;
use harness_code_analysis::{
    CallGraph, CallGraphCapabilities, CallGraphError, CallableResponse, CallsFromRequest,
    CallsResponse, CallsToRequest, FindCallablesRequest, RefreshRequest, RefreshResponse,
    TraversalRequest, TraversalResponse,
};
use tokio_util::sync::CancellationToken;

struct FakeCallGraph;

#[async_trait]
impl CallGraph for FakeCallGraph {
    async fn refresh(
        &self,
        _request: RefreshRequest,
        _cancel: CancellationToken,
    ) -> Result<RefreshResponse, CallGraphError> {
        Err(CallGraphError::Cancelled)
    }

    fn capabilities(&self) -> CallGraphCapabilities {
        CallGraphCapabilities {
            languages: Vec::new(),
            max_page_size: 1,
            max_traversal_depth: 1,
            supports_partial_results: false,
            supports_cross_file_resolution: false,
        }
    }

    async fn find_callables(
        &self,
        _request: FindCallablesRequest,
        _cancel: CancellationToken,
    ) -> Result<CallableResponse, CallGraphError> {
        Err(CallGraphError::Cancelled)
    }

    async fn calls_from(
        &self,
        _request: CallsFromRequest,
        _cancel: CancellationToken,
    ) -> Result<CallsResponse, CallGraphError> {
        Err(CallGraphError::Cancelled)
    }

    async fn calls_to(
        &self,
        _request: CallsToRequest,
        _cancel: CancellationToken,
    ) -> Result<CallsResponse, CallGraphError> {
        Err(CallGraphError::Cancelled)
    }

    async fn traverse(
        &self,
        _request: TraversalRequest,
        _cancel: CancellationToken,
    ) -> Result<TraversalResponse, CallGraphError> {
        Err(CallGraphError::Cancelled)
    }
}

#[tokio::test]
async fn consumers_can_use_the_public_contract_without_a_concrete_service() {
    let graph: Arc<dyn CallGraph> = Arc::new(FakeCallGraph);
    assert_eq!(graph.capabilities().max_page_size, 1);
    assert!(matches!(
        graph
            .refresh(
                RefreshRequest { max_files: Some(1) },
                CancellationToken::new()
            )
            .await,
        Err(CallGraphError::Cancelled)
    ));
}
