use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_protocol::backend::{
    ExecutionError, ExecutionEvent, ExecutionRequest, ExecutionResult,
};
use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::tools::{PermissionMode, ToolCapability, ToolPolicy};
use harness_runtime::traits::{EventSink, ExecutionBackend};

pub(crate) struct BroadcastEventSink {
    pub(crate) tx: broadcast::Sender<AgentEventEnvelope>,
}

impl EventSink for BroadcastEventSink {
    fn send(&self, envelope: AgentEventEnvelope) {
        let _ = self.tx.send(envelope);
    }
}

/// Ensures tools configured on the public session builder are advertised to
/// the backend even when the lower-level agent capability projection is empty.
pub(crate) struct ToolAdvertisingBackend {
    pub(crate) inner: Arc<dyn ExecutionBackend>,
    pub(crate) tools: Vec<harness_protocol::tools::ToolDescriptor>,
    pub(crate) selection: Option<harness_protocol::backend::PersistedBackendSelection>,
}

#[async_trait]
impl ExecutionBackend for ToolAdvertisingBackend {
    fn descriptor(&self) -> harness_protocol::backend::BackendDescriptor {
        let mut descriptor = self.inner.descriptor();
        if let Some(selection) = &self.selection {
            descriptor.name = format!(
                "{} [{}:{}]",
                descriptor.name, selection.provider, selection.provider_model_id
            );
        }
        descriptor
    }

    fn capabilities(&self) -> harness_protocol::backend::BackendCapabilities {
        self.inner.capabilities()
    }

    async fn execute(
        &self,
        mut request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        if request.tools.is_empty() {
            request.tools = self.tools.clone();
        }
        self.inner.execute(request, sink, cancel).await
    }
}

pub(crate) fn discovered_capability(
    descriptor: harness_tools::ToolDescriptor,
) -> (harness_protocol::ids::ToolId, ToolCapability) {
    let id = harness_protocol::ids::ToolId::new();
    (
        id,
        ToolCapability {
            descriptor: harness_protocol::tools::ToolDescriptor {
                id,
                name: descriptor.id.to_string(),
                description: descriptor.description,
                input_schema: descriptor.input_schema,
            },
            policy: ToolPolicy {
                permission: PermissionMode::Allow,
                enabled: true,
            },
            delegatable: false,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::backend::{BackendCapabilities, BackendDescriptor};
    use harness_protocol::ids::{BackendId, RequestId, RunId};
    use harness_protocol::tools::ToolDescriptor;
    use harness_runtime::traits::ExecutionBackend;

    struct RecordingBackend {
        seen: std::sync::Mutex<Vec<ExecutionRequest>>,
    }

    #[async_trait]
    impl ExecutionBackend for RecordingBackend {
        fn descriptor(&self) -> BackendDescriptor {
            BackendDescriptor {
                id: BackendId::new(),
                name: "test".into(),
                description: "test".into(),
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
                usage: Default::default(),
                cost: Default::default(),
                finish_reason: "test".into(),
            })
        }
    }

    fn request(tools: Vec<ToolDescriptor>) -> ExecutionRequest {
        ExecutionRequest {
            request_id: RequestId::new(),
            run_id: RunId::new(),
            system_prompt: String::new(),
            messages: Vec::new(),
            tools,
            extended_thinking: false,
            params: Default::default(),
        }
    }

    #[tokio::test]
    async fn advertises_tools_only_when_request_has_none() {
        let inner = Arc::new(RecordingBackend {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let advertised = ToolDescriptor {
            id: harness_protocol::ids::ToolId::new(),
            name: "advertised".into(),
            description: "test tool".into(),
            input_schema: serde_json::json!({}),
        };
        let backend = ToolAdvertisingBackend {
            inner: inner.clone(),
            tools: vec![advertised.clone()],
            selection: None,
        };
        let (tx, _) = broadcast::channel(1);
        backend
            .execute(request(Vec::new()), tx.clone(), CancellationToken::new())
            .await
            .unwrap();
        backend
            .execute(
                request(vec![ToolDescriptor {
                    name: "caller".into(),
                    ..advertised.clone()
                }]),
                tx,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let seen = inner.seen.lock().unwrap();
        assert_eq!(seen[0].tools.len(), 1);
        assert_eq!(seen[0].tools[0].name, advertised.name);
        assert_eq!(seen[1].tools[0].name, "caller");
    }

    #[test]
    fn discovered_capability_defaults_to_allowed_and_enabled() {
        let descriptor = harness_tools::ToolDescriptor {
            id: harness_tools::ToolId::new("demo"),
            name: "Demo".into(),
            description: "description".into(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let (id, capability) = discovered_capability(descriptor);
        assert_eq!(capability.descriptor.id, id);
        assert_eq!(capability.descriptor.name, "demo");
        assert_eq!(
            capability.policy.permission as u8,
            PermissionMode::Allow as u8
        );
        assert!(capability.policy.enabled);
        assert!(!capability.delegatable);
    }
}
