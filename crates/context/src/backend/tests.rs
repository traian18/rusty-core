use super::*;

use std::sync::Mutex;

use crate::provider::{
    ContextObservation, ContextObservationOrigin, ContextObservationPublisher, TrustedModelWindow,
};
use rusty_protocol::events::AgentEvent;
use rusty_protocol::ids::{AgentId, BackendId, RequestId, RunId, SessionId};
use rusty_protocol::usage::{Cost, ModelUsage};
use rusty_runtime::workspace::FakeWorkspace;

struct RecordingProvider {
    received_prompt: Mutex<Option<String>>,
    rewritten_prompt: String,
}

#[async_trait]
impl ContextProvider for RecordingProvider {
    async fn assemble(
        &self,
        mut request: ExecutionRequest,
        _workspace: &dyn Workspace,
    ) -> ExecutionRequest {
        *self.received_prompt.lock().unwrap() = Some(request.system_prompt.clone());
        request.system_prompt = self.rewritten_prompt.clone();
        request
    }
}

/// Records every request it receives and returns a canned success —
/// exists purely to prove `ContextAssemblingBackend` hands the *rewritten*
/// request to the inner backend, not the original.
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
        self.seen.lock().unwrap().push(request.clone());
        Ok(ExecutionResult {
            request_id: request.request_id,
            usage: ModelUsage::default(),
            cost: Cost::default(),
            finish_reason: "end_turn".to_string(),
        })
    }
}

fn request(system_prompt: &str) -> ExecutionRequest {
    ExecutionRequest {
        request_id: RequestId::new(),
        run_id: RunId::new(),
        system_prompt: system_prompt.to_string(),
        messages: vec![],
        tools: vec![],
        extended_thinking: false,
        params: Default::default(),
    }
}

struct RecordingPublisher {
    events: Mutex<Vec<AgentEvent>>,
}

#[async_trait]
impl ContextObservationPublisher for RecordingPublisher {
    async fn publish(&self, event: AgentEvent) -> bool {
        self.events.lock().unwrap().push(event);
        true
    }
}

#[tokio::test]
async fn default_observation_delegates_without_publishing() {
    let provider = RecordingProvider {
        received_prompt: Mutex::new(None),
        rewritten_prompt: "assembled prompt".to_string(),
    };
    let publisher = Arc::new(RecordingPublisher {
        events: Mutex::new(Vec::new()),
    });
    let request = request("original prompt");
    let observation = ContextObservation {
        origin: ContextObservationOrigin {
            session_id: SessionId::new(),
            agent_id: AgentId::new(),
            parent_agent_id: Some(AgentId::new()),
            run_id: request.run_id,
            request_id: request.request_id,
        },
        request_start_order: 1,
        trusted_model_window: Some(TrustedModelWindow {
            model: "test-model".to_string(),
            model_window: 8192,
        }),
        session_cancellation: CancellationToken::new(),
        publisher: publisher.clone(),
    };
    let provider_ref: &dyn ContextProvider = &provider;
    let assembled = provider_ref
        .assemble_with_observation(request, &FakeWorkspace::new(), &observation)
        .await;

    assert_eq!(
        *provider.received_prompt.lock().unwrap(),
        Some("original prompt".to_string())
    );
    assert_eq!(assembled.system_prompt, "assembled prompt");
    assert!(publisher.events.lock().unwrap().is_empty());
}

/// Request, session, agent, run and start order of an observation.
type Origin = (RequestId, SessionId, AgentId, RunId, u64);

struct OriginProvider {
    seen: Mutex<Vec<Origin>>,
}

#[async_trait]
impl ContextProvider for OriginProvider {
    async fn assemble(&self, request: ExecutionRequest, _: &dyn Workspace) -> ExecutionRequest {
        panic!("missing binding for {:?}", request.request_id)
    }

    async fn assemble_with_observation(
        &self,
        request: ExecutionRequest,
        _: &dyn Workspace,
        observation: &ContextObservation,
    ) -> ExecutionRequest {
        self.seen.lock().unwrap().push((
            observation.origin.request_id,
            observation.origin.session_id,
            observation.origin.agent_id,
            observation.origin.run_id,
            observation.request_start_order,
        ));
        request
    }
}

struct RuntimePublisher;

#[async_trait]
impl rusty_runtime::traits::RequestObservationPublisher for RuntimePublisher {
    async fn publish(&self, _: AgentEvent, _: RunId) -> bool {
        true
    }
}

#[tokio::test]
async fn wrapper_looks_up_each_request_origin_independently_in_reverse_order() {
    let bindings = rusty_runtime::traits::RequestObservationBindings::default();
    let first = request("first");
    let second = request("second");
    let origins = [
        (
            first.request_id,
            SessionId::new(),
            AgentId::new(),
            first.run_id,
            0,
        ),
        (
            second.request_id,
            SessionId::new(),
            AgentId::new(),
            second.run_id,
            1,
        ),
    ];
    for (request_id, session_id, agent_id, run_id, order) in origins {
        bindings.register(rusty_runtime::traits::RequestObservationBinding {
            request_id,
            session_id,
            agent_id,
            parent_agent_id: None,
            run_id,
            request_start_order: order,
            publisher: Arc::new(RuntimePublisher),
            session_cancellation: CancellationToken::new(),
        });
    }
    let provider = Arc::new(OriginProvider {
        seen: Mutex::new(Vec::new()),
    });
    let wrapped = ContextAssemblingBackend::new(
        Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        }),
        provider.clone(),
        Arc::new(FakeWorkspace::new()),
    )
    .with_observation_bindings(bindings);
    let (tx, _) = broadcast::channel(16);
    wrapped
        .execute(second, tx.clone(), CancellationToken::new())
        .await
        .unwrap();
    wrapped
        .execute(first, tx, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(*provider.seen.lock().unwrap(), vec![origins[1], origins[0]]);
}

#[tokio::test]
async fn provider_rewrites_request_before_inner_backend_sees_it() {
    let provider = Arc::new(RecordingProvider {
        received_prompt: Mutex::new(None),
        rewritten_prompt: "assembled prompt".to_string(),
    });
    let backend = Arc::new(RecordingBackend {
        seen: Mutex::new(Vec::new()),
    });
    let wrapped = ContextAssemblingBackend::new(
        backend.clone(),
        provider.clone(),
        Arc::new(FakeWorkspace::new()),
    );

    let (tx, _rx) = broadcast::channel(16);
    wrapped
        .execute(request("original prompt"), tx, CancellationToken::new())
        .await
        .expect("execute should succeed");

    assert_eq!(
        *provider.received_prompt.lock().unwrap(),
        Some("original prompt".to_string())
    );
    let seen = backend.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].system_prompt, "assembled prompt");
}

#[tokio::test]
async fn tool_pairs_survive_context_truncation() {
    use crate::providers::{StaticSystemPromptProvider, TruncatingCompactionProvider};
    use rusty_protocol::{
        ids::{MessageId, Timestamp, ToolCallId},
        messages::{AgentMessage, ContentBlock, MessageRole},
        tools::{ToolCall, ToolResultSummary},
    };
    let id = ToolCallId::new();
    let mut request = request("system");
    request.messages = vec![
        AgentMessage {
            id: MessageId::new(),
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolUse {
                call: ToolCall {
                    id,
                    name: "read".into(),
                    arguments: Default::default(),
                },
            }],
            created_at: Timestamp::now(),
        },
        AgentMessage {
            id: MessageId::new(),
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult {
                call_id: id,
                result: ToolResultSummary {
                    has_error: false,
                    output_preview: "important retained file contents".into(),
                },
            }],
            created_at: Timestamp::now(),
        },
    ];
    let backend = Arc::new(RecordingBackend {
        seen: Mutex::new(Vec::new()),
    });
    let provider = Arc::new(TruncatingCompactionProvider::new(
        Arc::new(StaticSystemPromptProvider::new("system")),
        1,
        1,
    ));
    let wrapped =
        ContextAssemblingBackend::new(backend.clone(), provider, Arc::new(FakeWorkspace::new()));
    let (tx, _) = broadcast::channel(16);
    wrapped
        .execute(request, tx, CancellationToken::new())
        .await
        .unwrap();
    let seen = backend.seen.lock().unwrap();
    // The pair-safe cut keeps the ToolUse with its ToolResult, so nothing
    // is orphaned and the result's content reaches the backend intact.
    let blocks: Vec<_> = seen[0].messages.iter().flat_map(|m| &m.content).collect();
    assert!(blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { call } if call.id == id)));
    assert!(blocks.iter().any(|b| matches!(
        b,
        ContentBlock::ToolResult { call_id, result }
            if *call_id == id && result.output_preview.contains("important retained file contents")
    )));
}

/// Records what the runtime publisher is given, as the runner would see it.
#[derive(Default)]
struct RecordingRuntimePublisher {
    events: Mutex<Vec<AgentEvent>>,
}

#[async_trait]
impl rusty_runtime::traits::RequestObservationPublisher for RecordingRuntimePublisher {
    async fn publish(&self, event: AgentEvent, _: RunId) -> bool {
        self.events.lock().unwrap().push(event);
        true
    }
}

fn bound(
    request: &ExecutionRequest,
    publisher: Arc<RecordingRuntimePublisher>,
) -> rusty_runtime::traits::RequestObservationBindings {
    let bindings = rusty_runtime::traits::RequestObservationBindings::default();
    bindings.register(rusty_runtime::traits::RequestObservationBinding {
        session_id: SessionId::new(),
        agent_id: AgentId::new(),
        parent_agent_id: None,
        run_id: request.run_id,
        request_id: request.request_id,
        request_start_order: 4,
        publisher,
        session_cancellation: CancellationToken::new(),
    });
    bindings
}

#[tokio::test]
async fn the_estimate_describes_the_request_after_compaction_and_repair() {
    use crate::providers::{estimate_tokens, PolicyDrivenCompactionProvider};
    use rusty_protocol::events::ContextAvailability;
    use rusty_protocol::ids::{MessageId, Timestamp};
    use rusty_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
    let mut request = request("");
    request.params.model = Some("m".into());
    request.messages = (0..6)
        .map(|i| AgentMessage {
            id: MessageId::new(),
            role: if i % 2 == 0 {
                MessageRole::User
            } else {
                MessageRole::Assistant
            },
            content: vec![ContentBlock::Text {
                text: "word ".repeat(6_000),
            }],
            created_at: Timestamp::now(),
        })
        .collect();
    let original = estimate_tokens(&request).tokens;
    let publisher = Arc::new(RecordingRuntimePublisher::default());
    let backend = Arc::new(RecordingBackend {
        seen: Mutex::new(Vec::new()),
    });
    let compactor = Arc::new(PolicyDrivenCompactionProvider::new(
        Arc::new(crate::providers::ChainedContextProvider::new(vec![])),
        crate::policy::ContextPolicy::default(),
        Some(50_000),
        1,
        10_000,
    ));
    let wrapped =
        ContextAssemblingBackend::new(backend.clone(), compactor, Arc::new(FakeWorkspace::new()))
            .with_observation_bindings(bound(&request, publisher.clone()))
            .with_trusted_model_window(Some(TrustedModelWindow {
                model: "m".into(),
                model_window: 50_000,
            }));
    let (tx, _) = broadcast::channel(16);
    wrapped
        .execute(request, tx, CancellationToken::new())
        .await
        .unwrap();
    let sent = estimate_tokens(&backend.seen.lock().unwrap()[0]).tokens;
    let events = publisher.events.lock().unwrap().clone();
    let Some(AgentEvent::ContextUpdated {
        availability:
            ContextAvailability::Available {
                estimated_input_tokens,
                input_budget,
                remaining_input_tokens,
                ..
            },
        request_start_order,
        ..
    }) = events.last()
    else {
        panic!("expected a final estimate, got {events:?}");
    };
    assert!(sent < original);
    assert_eq!(
        *estimated_input_tokens, sent,
        "the sent request, not the original"
    );
    assert_eq!(*remaining_input_tokens, input_budget.saturating_sub(sent));
    assert_eq!(*request_start_order, 4);
    assert!(matches!(events[0], AgentEvent::CompactionStarted { .. }));
}

#[tokio::test]
async fn a_request_without_a_binding_publishes_nothing() {
    let publisher = Arc::new(RecordingRuntimePublisher::default());
    let wrapped = ContextAssemblingBackend::new(
        Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        }),
        Arc::new(crate::providers::ChainedContextProvider::new(vec![])),
        Arc::new(FakeWorkspace::new()),
    )
    .with_observation_bindings(bound(&request("other"), publisher.clone()));
    let (tx, _) = broadcast::channel(16);
    wrapped
        .execute(request("unbound"), tx, CancellationToken::new())
        .await
        .unwrap();
    assert!(publisher.events.lock().unwrap().is_empty());
}

/// A provider that never finishes assembling.
struct StuckProvider;

#[async_trait]
impl ContextProvider for StuckProvider {
    async fn assemble(&self, request: ExecutionRequest, _: &dyn Workspace) -> ExecutionRequest {
        std::future::pending::<()>().await;
        request
    }
}

#[tokio::test]
async fn cancelling_a_request_does_not_wait_for_a_stuck_provider() {
    let backend = Arc::new(RecordingBackend {
        seen: Mutex::new(Vec::new()),
    });
    let wrapped = ContextAssemblingBackend::new(
        backend.clone(),
        Arc::new(StuckProvider),
        Arc::new(FakeWorkspace::new()),
    );
    let cancel = CancellationToken::new();
    let (tx, _) = broadcast::channel(16);
    let running = wrapped.execute(request("p"), tx, cancel.clone());
    cancel.cancel();
    assert!(matches!(running.await, Err(ExecutionError::Cancelled)));
    assert!(backend.seen.lock().unwrap().is_empty());
}
