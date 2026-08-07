//! Integration test: `SessionBuilder::context_provider(...)` actually reaches
//! the backend — i.e. `.toolset()`'s `ToolAdvertisingBackend` and
//! `.context_provider()`'s `ContextAssemblingBackend` compose correctly
//! rather than one clobbering the other.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_context::{ContextProvider, StaticSystemPromptProvider};
use harness_engine::Harness;
use harness_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
    ExecutionResult,
};
use harness_protocol::ids::BackendId;
use harness_protocol::usage::{Cost, ModelUsage};
use harness_runtime::traits::ExecutionBackend;
use harness_tools::registry::ToolRegistry;
use harness_tools::ToolDescriptor;

/// Records the exact `ExecutionRequest` it received and returns a canned
/// success — exists to prove what the backend actually saw after every
/// decorator (tool advertising, context assembly) has run.
struct RecordingBackend {
    seen: Mutex<Vec<ExecutionRequest>>,
}

#[async_trait]
impl ExecutionBackend for RecordingBackend {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: BackendId::new(),
            name: "recording".to_string(),
            description: "test double".to_string(),
            capabilities: BackendCapabilities::default(),
        }
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::default()
    }

    async fn execute(
        &self,
        request: ExecutionRequest,
        _sink: broadcast::Sender<ExecutionEvent>,
        _cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        let request_id = request.request_id;
        self.seen.lock().unwrap().push(request);
        Ok(ExecutionResult {
            request_id,
            usage: ModelUsage::default(),
            cost: Cost::default(),
            finish_reason: "end_turn".to_string(),
        })
    }
}

struct NoTools;

#[async_trait]
impl ToolRegistry for NoTools {
    fn register(
        &self,
        _executor: Arc<dyn harness_tools::ToolExecutor>,
    ) -> Result<(), harness_tools::registry::RegistrationError> {
        Ok(())
    }

    fn get_executor(&self, _tool_id: &str) -> Option<Arc<dyn harness_tools::ToolExecutor>> {
        None
    }

    fn descriptors(&self) -> Vec<ToolDescriptor> {
        vec![]
    }
}

#[tokio::test]
async fn context_provider_rewrites_the_request_the_backend_sees() {
    let backend = Arc::new(RecordingBackend {
        seen: Mutex::new(Vec::new()),
    });
    let provider: Arc<dyn ContextProvider> =
        Arc::new(StaticSystemPromptProvider::new("project instructions"));

    let handle = Harness::new()
        .session()
        .backend(backend.clone())
        .tools(Arc::new(NoTools))
        .context_provider(provider)
        .start()
        .await
        .expect("SessionBuilder::start() should succeed");

    handle
        .send("hello from test")
        .await
        .expect("SessionHandle::send() should succeed");

    // Give the async run a moment to reach the backend.
    for _ in 0..50 {
        if !backend.seen.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let seen = backend.seen.lock().unwrap();
    assert_eq!(
        seen.len(),
        1,
        "backend should have received exactly one request"
    );
    assert_eq!(seen[0].system_prompt, "project instructions");
}
