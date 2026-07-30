//! Public session builder and live session handle.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use tokio::sync::broadcast;

use harness_protocol::backend::{ExecutionError, ExecutionEvent, ExecutionRequest, ExecutionResult};
use harness_protocol::commands::UserInput;
use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::SessionId;
use harness_protocol::tools::ToolDescriptor;
use tokio_util::sync::CancellationToken;
use harness_runtime::session_client::{SessionClient, SessionSnapshot};
use harness_runtime::session_runtime::{SessionCommand, SessionError, SessionRuntime};
use harness_runtime::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};
use harness_runtime::workspace::FakeWorkspace;
use harness_runtime::{IntegrationError, IntegrationRegistry};

struct BroadcastEventSink {
    tx: broadcast::Sender<AgentEventEnvelope>,
}

/// Ensures tools configured on the public session builder are advertised to
/// the backend even when the lower-level agent capability projection is empty.
struct ToolAdvertisingBackend {
    inner: Arc<dyn ExecutionBackend>,
    tools: Vec<ToolDescriptor>,
}

#[async_trait]
impl ExecutionBackend for ToolAdvertisingBackend {
    fn descriptor(&self) -> harness_protocol::backend::BackendDescriptor {
        self.inner.descriptor()
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

impl EventSink for BroadcastEventSink {
    fn send(&self, envelope: AgentEventEnvelope) {
        let _ = self.tx.send(envelope);
    }
}

/// Errors raised while configuring or operating a session.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("missing required field: backend")]
    MissingBackend,
    #[error("missing required field: tool_registry")]
    MissingToolRegistry,
    #[error("invalid integration configuration: {0}")]
    InvalidIntegrationConfig(#[from] serde_json::Error),
    #[error("integration error: {0}")]
    Integration(#[from] IntegrationError),
    #[error("session error: {0}")]
    Session(#[from] SessionError),
}

struct PendingIntegration {
    id: String,
    config: serde_json::Value,
}

/// Fluent builder for direct or registry-backed sessions.
pub struct SessionBuilder {
    backend: Option<Arc<dyn ExecutionBackend>>,
    integration: Option<PendingIntegration>,
    integrations: Arc<IntegrationRegistry>,
    tool_registry: Option<Arc<dyn ToolRegistry>>,
    workspace: Option<Arc<dyn Workspace>>,
}

impl SessionBuilder {
    /// Create a builder with an empty integration registry.
    pub fn new() -> Self {
        Self::with_integrations(Arc::new(IntegrationRegistry::new()))
    }

    /// Create a builder attached to a shared registry.
    pub fn with_integrations(integrations: Arc<IntegrationRegistry>) -> Self {
        Self {
            backend: None,
            integration: None,
            integrations,
            tool_registry: None,
            workspace: None,
        }
    }

    /// Inject an already constructed backend.
    pub fn backend(mut self, backend: Arc<dyn ExecutionBackend>) -> Self {
        self.backend = Some(backend);
        self.integration = None;
        self
    }

    /// Select a registered integration and provider-specific configuration.
    ///
    /// Resolution is deferred until [`start`](Self::start), keeping the fluent
    /// builder synchronous while allowing factories to perform async setup.
    pub fn integration<C: Serialize>(
        mut self,
        id: impl Into<String>,
        config: C,
    ) -> Result<Self, HarnessError> {
        self.integration = Some(PendingIntegration {
            id: id.into(),
            config: serde_json::to_value(config)?,
        });
        self.backend = None;
        Ok(self)
    }

    /// Set the session tool registry.
    pub fn tools(mut self, tool_registry: Arc<dyn ToolRegistry>) -> Self {
        self.tool_registry = Some(tool_registry);
        self
    }

    /// Set the workspace, defaulting to an empty in-memory workspace.
    pub fn workspace(mut self, workspace: Arc<dyn Workspace>) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// Resolve configuration and create the live session.
    pub async fn start(self) -> Result<SessionHandle, HarnessError> {
        let backend = match (self.backend, self.integration) {
            (Some(backend), _) => backend,
            (None, Some(integration)) => {
                self.integrations
                    .create(&integration.id, integration.config)
                    .await?
            }
            (None, None) => return Err(HarnessError::MissingBackend),
        };
        let tool_registry = self
            .tool_registry
            .ok_or(HarnessError::MissingToolRegistry)?;
        let backend: Arc<dyn ExecutionBackend> = Arc::new(ToolAdvertisingBackend {
            inner: backend,
            tools: tool_registry.descriptors(),
        });
        let workspace: Arc<dyn Workspace> = self
            .workspace
            .unwrap_or_else(|| Arc::new(FakeWorkspace::new()));
        let session_id = SessionId::new();
        let (event_tx, _) = broadcast::channel(256);
        let event_sink = Arc::new(BroadcastEventSink {
            tx: event_tx.clone(),
        });
        let runtime = Arc::new(SessionRuntime::new(
            session_id,
            backend,
            tool_registry,
            workspace,
            event_sink,
        ));

        Ok(SessionHandle {
            client: SessionClient::new(runtime),
            session_id,
        })
    }
}

impl Default for SessionBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle to a live session.
pub struct SessionHandle {
    client: SessionClient,
    session_id: SessionId,
}

impl SessionHandle {
    pub async fn send(&self, prompt: &str) -> Result<(), HarnessError> {
        self.client
            .send(SessionCommand::Prompt(UserInput {
                text: prompt.to_string(),
                attachments: vec![],
            }))
            .await?;
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEventEnvelope> {
        self.client.subscribe()
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        self.client.snapshot()
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }
}
