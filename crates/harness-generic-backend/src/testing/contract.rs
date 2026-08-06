//! Reusable smoke-level contract checks for execution backends.

use std::sync::Arc;

use harness_protocol::backend::{ExecutionError, ExecutionRequest};
use harness_protocol::ids::{RequestId, RunId};
use harness_runtime::traits::ExecutionBackend;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

fn request() -> ExecutionRequest {
    ExecutionRequest {
        request_id: RequestId::new(),
        run_id: RunId::new(),
        system_prompt: String::new(),
        messages: Vec::new(),
        tools: Vec::new(),
        extended_thinking: false,
    }
}

/// Run provider-neutral descriptor, execution, and cancellation checks.
///
/// Provider crates can call this function directly from their own integration
/// tests without copying the generic backend test implementation.
pub async fn run_backend_contract_suite(backend: Arc<dyn ExecutionBackend>) {
    let descriptor = backend.descriptor();
    assert!(!descriptor.id.to_string().is_empty(), "backend id must be set");
    assert!(!descriptor.name.is_empty(), "backend name must be set");
    let _ = backend.capabilities();

    let (events, _receiver) = broadcast::channel(256);
    if let Ok(result) = backend
        .execute(request(), events, CancellationToken::new())
        .await
    {
        assert!(
            !result.finish_reason.is_empty(),
            "successful execution must have a finish reason"
        );
    }

    let (events, _receiver) = broadcast::channel(256);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    match backend.execute(request(), events, cancellation).await {
        Ok(_) | Err(ExecutionError::Cancelled) => {}
        Err(error) => panic!("cancelled execution returned the wrong error: {error:?}"),
    }
}
