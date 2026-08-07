//! M4: end-to-end coverage for `SessionBuilder::execution_params` /
//! `SessionHandle::set_execution_params` and `Harness::validate_model_override`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_engine::Harness;
use harness_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionParams,
    ExecutionRequest, ExecutionResult,
};
use harness_protocol::usage::{Cost, ModelUsage};
use harness_runtime::traits::ExecutionBackend;
use harness_tools::registry::ToolRegistry;
use harness_tools::ToolDescriptor;

/// Records the `ExecutionParams` of every `ExecutionRequest` it receives, so
/// a test can assert what the deterministic core actually built without
/// needing a real provider client.
struct RecordingBackend {
    descriptor: BackendDescriptor,
    seen: Arc<Mutex<Vec<ExecutionParams>>>,
}

impl RecordingBackend {
    fn new(seen: Arc<Mutex<Vec<ExecutionParams>>>) -> Self {
        Self {
            descriptor: BackendDescriptor {
                id: harness_protocol::ids::BackendId::new(),
                name: "recording".into(),
                description: "Records ExecutionParams for M4 tests".into(),
                capabilities: BackendCapabilities {
                    streaming: true,
                    ..Default::default()
                },
            },
            seen,
        }
    }
}

#[async_trait]
impl ExecutionBackend for RecordingBackend {
    fn descriptor(&self) -> BackendDescriptor {
        self.descriptor.clone()
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.descriptor.capabilities.clone()
    }

    async fn execute(
        &self,
        request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        _cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        self.seen
            .lock()
            .expect("seen mutex poisoned")
            .push(request.params.clone());
        let result = ExecutionResult {
            request_id: request.request_id,
            usage: ModelUsage::default(),
            cost: Cost::default(),
            finish_reason: "end_turn".into(),
        };
        let _ = sink.send(ExecutionEvent::Completed {
            request_id: request.request_id,
            result: result.clone(),
        });
        Ok(result)
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

async fn wait_for_completion(rx: &mut broadcast::Receiver<harness_protocol::events::AgentEventEnvelope>) {
    for _ in 0..50 {
        while let Ok(envelope) = rx.try_recv() {
            if matches!(envelope.event, harness_protocol::events::AgentEvent::Completed { .. }) {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for Completed event");
}

/// `SessionBuilder::execution_params` must apply before the handle is
/// returned, so the very first prompt already carries it.
#[tokio::test]
async fn session_builder_execution_params_apply_to_the_first_prompt() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(RecordingBackend::new(seen.clone()));

    let handle = Harness::new()
        .session()
        .backend(backend)
        .tools(Arc::new(NoTools))
        .execution_params(ExecutionParams {
            model: Some("claude-opus-4-20250514".to_string()),
            max_tokens: Some(4096),
            ..Default::default()
        })
        .start()
        .await
        .expect("start should succeed");

    let mut rx = handle.subscribe();
    handle.send("hello").await.expect("send should succeed");
    wait_for_completion(&mut rx).await;

    let recorded = seen.lock().expect("seen mutex poisoned");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].model.as_deref(), Some("claude-opus-4-20250514"));
    assert_eq!(recorded[0].max_tokens, Some(4096));
}

/// `SessionHandle::set_execution_params` changes what the *next* prompt
/// carries without needing to recreate the session, and is a partial update
/// (fields left unset keep their previous value).
#[tokio::test]
async fn set_execution_params_changes_the_next_prompt_and_preserves_unset_fields() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(RecordingBackend::new(seen.clone()));

    let handle = Harness::new()
        .session()
        .backend(backend)
        .tools(Arc::new(NoTools))
        .execution_params(ExecutionParams {
            model: Some("claude-sonnet-4-20250514".to_string()),
            max_tokens: Some(4096),
            ..Default::default()
        })
        .start()
        .await
        .expect("start should succeed");

    let mut rx = handle.subscribe();
    handle.send("first").await.expect("send should succeed");
    wait_for_completion(&mut rx).await;

    // Only change temperature; model/max_tokens must survive into run 2.
    handle
        .set_execution_params(ExecutionParams {
            temperature: Some(0.9),
            ..Default::default()
        })
        .await
        .expect("set_execution_params should succeed");

    handle.send("second").await.expect("send should succeed");
    wait_for_completion(&mut rx).await;

    let recorded = seen.lock().expect("seen mutex poisoned");
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].temperature, None);
    assert_eq!(recorded[1].model.as_deref(), Some("claude-sonnet-4-20250514"));
    assert_eq!(recorded[1].max_tokens, Some(4096));
    assert_eq!(recorded[1].temperature, Some(0.9));
}

/// `Harness::validate_model_override` rejects a model id that isn't in the
/// provider's known catalog, and accepts one that is — using the built-in
/// `anthropic-api` default catalog so the test needs no network access or
/// credentials.
#[test]
fn validate_model_override_checks_the_known_catalog() {
    let harness = Harness::new();
    let provider = harness_engine::providers::ProviderKey::new("anthropic-api");

    harness
        .validate_model_override(&provider, "claude-sonnet-4-20250514")
        .expect("a real default-catalog model id must validate");

    let error = harness
        .validate_model_override(&provider, "definitely-not-a-real-model")
        .expect_err("an unknown model id must be rejected");
    assert!(matches!(
        error,
        harness_engine::HarnessError::UnknownModel { .. }
    ));
}
