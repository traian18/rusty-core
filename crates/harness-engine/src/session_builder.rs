//! Public session builder and live session handle.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_protocol::backend::{
    ExecutionError, ExecutionEvent, ExecutionRequest, ExecutionResult,
};
use harness_protocol::commands::{PermissionDecision, UserInput};
use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::{PermissionId, SessionId};
use harness_protocol::tools::{AgentToolset, PermissionMode, ToolCapability, ToolPolicy};
use harness_runtime::session_client::{SessionClient, SessionSnapshot};
use harness_runtime::session_manager::{SessionManager, SessionManagerError};
use harness_runtime::session_runtime::{SessionCommand, SessionError, SessionRuntime};
use harness_runtime::traits::{EventSink, ExecutionBackend, ToolRegistry};
use harness_runtime::workspace::FakeWorkspace;
use harness_runtime::{IntegrationError, IntegrationRegistry};

use harness_tool_filesystem::{EditTool, ReadTool, SearchTool};
use harness_tool_git::{GitDiffTool, GitLogTool, GitShowTool, GitStatusTool};
use harness_tool_shell::ExecTool;
use harness_tool_web::FetchTool;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

struct BroadcastEventSink {
    tx: broadcast::Sender<AgentEventEnvelope>,
}

/// Ensures tools configured on the public session builder are advertised to
/// the backend even when the lower-level agent capability projection is empty.
struct ToolAdvertisingBackend {
    inner: Arc<dyn ExecutionBackend>,
    tools: Vec<harness_protocol::tools::ToolDescriptor>,
    selection: Option<harness_protocol::backend::PersistedBackendSelection>,
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

impl EventSink for BroadcastEventSink {
    fn send(&self, envelope: AgentEventEnvelope) {
        let _ = self.tx.send(envelope);
    }
}

/// Errors raised while configuring or operating a session.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("unknown provider: {0}")]
    UnknownProvider(String),
    #[error("provider catalog error: {0}")]
    ProviderCatalog(String),
    #[error("unknown model {model_id:?} for provider {provider}")]
    UnknownModel { provider: String, model_id: String },
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
    #[error("session manager error: {0}")]
    SessionManager(#[from] SessionManagerError),
    #[error("session store error: {0}")]
    Store(#[from] harness_session_store::StoreError),
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
    workspace: Option<Arc<dyn harness_runtime::traits::Workspace>>,
    /// The toolset to inject into the root agent's capabilities.
    /// When set alongside `tool_registry`, the builder creates the registry
    /// from the toolset's enabled descriptors via `build_executor_for()`.
    root_toolset: Option<AgentToolset>,
    /// The shared [`SessionManager`] that owns all active sessions.
    /// When `None`, a default manager is created in [`start`](Self::start).
    session_manager: Option<Arc<SessionManager>>,
    /// Optional context assembly/compaction provider — see
    /// [`context_provider`](Self::context_provider).
    context_provider: Option<Arc<dyn harness_context::ContextProvider>>,
    /// Session-level default execution params (model, max_tokens,
    /// temperature, reasoning, ...) applied immediately after the session
    /// starts — see [`execution_params`](Self::execution_params).
    execution_params: Option<harness_protocol::backend::ExecutionParams>,
}

impl SessionBuilder {
    /// Create a builder with an empty integration registry and a default
    /// [`SessionManager`].
    pub fn new() -> Self {
        Self::with_integrations(Arc::new(IntegrationRegistry::new()))
    }

    /// Create a builder attached to a shared integration registry.
    ///
    /// A fresh [`SessionManager`] is created internally.
    pub fn with_integrations(integrations: Arc<IntegrationRegistry>) -> Self {
        Self {
            backend: None,
            integration: None,
            integrations,
            tool_registry: None,
            workspace: None,
            root_toolset: None,
            session_manager: None,
            context_provider: None,
            execution_params: None,
        }
    }

    /// Create a builder attached to both a shared integration registry and
    /// a shared [`SessionManager`].
    ///
    /// This is the constructor used by [`Harness`](crate::Harness) so that
    /// all sessions created through the harness are tracked in the same
    /// manager.
    pub fn with_integrations_and_manager(
        integrations: Arc<IntegrationRegistry>,
        session_manager: Arc<SessionManager>,
    ) -> Self {
        Self {
            backend: None,
            integration: None,
            integrations,
            tool_registry: None,
            workspace: None,
            root_toolset: None,
            session_manager: Some(session_manager),
            context_provider: None,
            execution_params: None,
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
    pub fn workspace(mut self, workspace: Arc<dyn harness_runtime::traits::Workspace>) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// Set the session's default model/execution params (model override,
    /// max_tokens, temperature, reasoning effort, ...).
    ///
    /// Applied immediately after the session starts, before the handle is
    /// returned — so the very first prompt already uses these params. Call
    /// [`SessionHandle::set_execution_params`] later to change them mid-session
    /// (e.g. a per-run override before the next prompt).
    pub fn execution_params(mut self, params: harness_protocol::backend::ExecutionParams) -> Self {
        self.execution_params = Some(params);
        self
    }

    /// Install a context-assembly/compaction provider.
    ///
    /// When set, [`start`](Self::start) wraps the resolved backend in a
    /// [`harness_context::ContextAssemblingBackend`] — every request's
    /// `system_prompt`/`messages` are rewritten by `provider` before
    /// reaching the backend. Mirrors how [`toolset`](Self::toolset) wraps
    /// the backend in `ToolAdvertisingBackend`; see
    /// `crates/harness-context/PLAN.md` for why this is a backend decorator
    /// rather than a change to `harness-core`.
    pub fn context_provider(mut self, provider: Arc<dyn harness_context::ContextProvider>) -> Self {
        self.context_provider = Some(provider);
        self
    }

    /// Register an `AgentToolset` into the session.
    ///
    /// This is an alternative to [`tools`](Self::tools) that takes a
    /// high-level tool policy set and a [`Workspace`](harness_runtime::traits::Workspace)
    /// reference, creates the appropriate [`ToolExecutor`](harness_runtime::traits::ToolExecutor)
    /// instances via `build_executor_for`, registers them into a
    /// [`SimpleToolRegistry`](harness_runtime::traits::SimpleToolRegistry), and wires both the
    /// registry and the toolset into the root agent's capabilities.
    ///
    /// When this method is used, the builder will automatically populate
    /// the tool registry — there is no need to also call [`tools`](Self::tools).
    ///
    /// The `workspace` argument provides the [`Workspace`](harness_runtime::traits::Workspace)
    /// that filesystem tools (`fs.read`, `fs.edit`, `workspace.search`) will delegate to.
    /// Shell tools (`shell.exec`) ignore the workspace.
    pub fn toolset(
        mut self,
        toolset: AgentToolset,
        workspace: Arc<dyn harness_runtime::traits::Workspace>,
    ) -> Self {
        // 1. Register all tool executors into the registry.
        let registry = harness_runtime::traits::SimpleToolRegistry::new();
        for descriptor in toolset.enabled_descriptors() {
            let executor = Self::build_executor_for(descriptor, workspace.clone());
            let _ = registry.register(executor);
        }

        // 2. Wire the registry + toolset into the root Agent's capabilities.
        self.tool_registry = Some(Arc::new(registry));
        self.root_toolset = Some(toolset);
        self.workspace = Some(workspace);
        self
    }

    /// Build the appropriate executor for a given tool descriptor.
    ///
    /// Maps known descriptor name strings to concrete tool implementations.
    pub(crate) fn build_executor_for(
        descriptor: &harness_protocol::tools::ToolDescriptor,
        workspace: Arc<dyn harness_runtime::traits::Workspace>,
    ) -> Arc<dyn harness_runtime::traits::ToolExecutor> {
        match descriptor.name.as_str() {
            "fs.read" => Arc::new(ReadTool::new(workspace)),
            "fs.edit" => Arc::new(EditTool::new(workspace)),
            "workspace.search" => Arc::new(SearchTool::new(workspace)),
            "shell.exec" => Arc::new(ExecTool::new()),
            // Git tools resolve the repo directly from the workspace root
            // via git2::Repository::discover — they don't go through the
            // Workspace trait's virtual filesystem access.
            "git.status" => Arc::new(GitStatusTool::new(workspace.root().to_path_buf())),
            "git.diff" => Arc::new(GitDiffTool::new(workspace.root().to_path_buf())),
            "git.log" => Arc::new(GitLogTool::new(workspace.root().to_path_buf())),
            "git.show" => Arc::new(GitShowTool::new(workspace.root().to_path_buf())),
            "web.fetch" => Arc::new(FetchTool::new()),
            _ => {
                // Create a fallback that always fails
                // Convert protocol descriptor to harness-tools descriptor
                let tool_desc = harness_tools::ToolDescriptor {
                    id: harness_tools::ToolId::new(&descriptor.name),
                    name: descriptor.name.clone(),
                    description: descriptor.description.clone(),
                    input_schema: descriptor.input_schema.clone(),
                };
                Arc::new(harness_tools::UnknownTool {
                    descriptor: tool_desc,
                })
            }
        }
    }

    /// Resolve configuration and create the live session.
    ///
    /// Session creation is routed through the [`SessionManager`], which
    /// handles ID generation, runtime construction, and supervisor task
    /// spawning. The resulting `Arc<SessionRuntime>` is registered with
    /// the manager for centralized lifecycle control.
    pub async fn start(self) -> Result<SessionHandle, HarnessError> {
        let (backend, persisted_selection) = match (self.backend, self.integration) {
            (Some(backend), _) => (backend, None),
            (None, Some(integration)) => {
                let persisted_selection = integration
                    .config
                    .get("_backend_selection")
                    .and_then(|value| serde_json::from_value(value.clone()).ok());
                let backend = self
                    .integrations
                    .create(&integration.id, integration.config)
                    .await?;
                (backend, persisted_selection)
            }
            (None, None) => return Err(HarnessError::MissingBackend),
        };
        let tool_registry = self
            .tool_registry
            .ok_or(HarnessError::MissingToolRegistry)?;

        // A high-level toolset carries explicit policy. For callers that
        // provide a registry directly, derive an enabled/allowed toolset so
        // registered tools are also executable by the root agent.
        let root_toolset = self.root_toolset.unwrap_or_else(|| {
            let tools = tool_registry
                .descriptors()
                .into_iter()
                .map(|desc| {
                    let id = harness_protocol::ids::ToolId::new();
                    (
                        id,
                        ToolCapability {
                            descriptor: harness_protocol::tools::ToolDescriptor {
                                id,
                                name: desc.id.to_string(),
                                description: desc.description,
                                input_schema: desc.input_schema,
                            },
                            policy: ToolPolicy {
                                permission: PermissionMode::Allow,
                                enabled: true,
                            },
                            delegatable: false,
                        },
                    )
                })
                .collect();
            AgentToolset { tools }
        });
        let protocol_descriptors = root_toolset
            .enabled_descriptors()
            .into_iter()
            .cloned()
            .collect();

        let backend: Arc<dyn ExecutionBackend> = Arc::new(ToolAdvertisingBackend {
            inner: backend,
            tools: protocol_descriptors,
            selection: persisted_selection,
        });
        let workspace: Arc<dyn harness_runtime::traits::Workspace> = self
            .workspace
            .unwrap_or_else(|| Arc::new(FakeWorkspace::new()));

        let backend: Arc<dyn ExecutionBackend> = match self.context_provider {
            Some(provider) => Arc::new(harness_context::ContextAssemblingBackend::new(
                backend,
                provider,
                workspace.clone(),
            )),
            None => backend,
        };

        let (event_tx, _) = broadcast::channel(256);
        let event_sink = Arc::new(BroadcastEventSink {
            tx: event_tx.clone(),
        });

        // Use the provided SessionManager or create a default one.
        let session_manager = self
            .session_manager
            .unwrap_or_else(|| Arc::new(SessionManager::default()));

        let runtime = session_manager
            .create_session(backend, tool_registry, workspace, event_sink, root_toolset)
            .await?;
        runtime.integrations.extend_from(&self.integrations)?;
        let session_id = runtime.session_id;

        let client = SessionClient::new(runtime);
        if let Some(params) = self.execution_params {
            client
                .send(SessionCommand::ConfigureExecution(params))
                .await?;
        }

        Ok(SessionHandle {
            client,
            session_id,
            session_manager,
        })
    }
}

impl Default for SessionBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle to a live session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextInspection {
    pub generation: u64,
    pub estimated_tokens: Option<u64>,
    pub checkpoint: Option<String>,
    pub covered_through: Option<String>,
    pub pinned_items: usize,
    pub last_compacted_at: Option<String>,
}

pub struct SessionHandle {
    client: SessionClient,
    session_id: SessionId,
    session_manager: Arc<SessionManager>,
}

impl SessionHandle {
    /// Wraps an already-running session runtime as a live [`SessionHandle`].
    ///
    /// This is the restore path's counterpart to
    /// [`SessionBuilder::start`](SessionBuilder::start): instead of creating
    /// a fresh session, it exposes a runtime that was rebuilt from a
    /// persisted snapshot (see
    /// [`Harness::restore_session`](crate::Harness::restore_session)) through
    /// the same handle API.
    pub(crate) fn from_runtime(
        runtime: Arc<SessionRuntime>,
        session_manager: Arc<SessionManager>,
    ) -> Self {
        let session_id = runtime.session_id;
        Self {
            client: SessionClient::new(runtime),
            session_id,
            session_manager,
        }
    }

    pub async fn send(&self, prompt: &str) -> Result<(), HarnessError> {
        self.send_input(UserInput {
            text: prompt.to_string(),
            attachments: vec![],
        })
        .await?;
        Ok(())
    }

    /// Send a complete user input, preserving any supplied attachments.
    pub async fn send_input(&self, input: UserInput) -> Result<(), HarnessError> {
        self.client.send(SessionCommand::Prompt(input)).await?;
        Ok(())
    }

    /// Inject additional user input into this session.
    pub async fn steer(&self, prompt: &str) -> Result<(), HarnessError> {
        self.steer_input(UserInput {
            text: prompt.to_string(),
            attachments: vec![],
        })
        .await?;
        Ok(())
    }

    /// Inject complete user input, preserving any supplied attachments.
    pub async fn steer_input(&self, input: UserInput) -> Result<(), HarnessError> {
        self.client.steer(input).await?;
        Ok(())
    }

    /// Queue a prompt to run after the current command boundary.
    pub async fn follow_up(&self, prompt: &str) -> Result<(), HarnessError> {
        self.follow_up_input(UserInput {
            text: prompt.to_string(),
            attachments: vec![],
        })
        .await?;
        Ok(())
    }

    /// Queue complete user input, preserving any supplied attachments.
    pub async fn follow_up_input(&self, input: UserInput) -> Result<(), HarnessError> {
        self.client.follow_up(input).await?;
        Ok(())
    }

    /// Update this session's default model/execution params.
    ///
    /// Applied as a partial update — fields left unset in `params` keep
    /// their previous value (see `ExecutionParams::merge_over`). Takes
    /// effect starting with the next prompt/steer/follow-up; never mutates
    /// an already-in-flight run. Use this both to change the session's
    /// standing default (e.g. "switch this session to opus") and, sent
    /// immediately before one `send`, as a one-off override for the next
    /// run only if you follow it with another call reverting the field.
    pub async fn set_execution_params(
        &self,
        params: harness_protocol::backend::ExecutionParams,
    ) -> Result<(), HarnessError> {
        self.client
            .send(SessionCommand::ConfigureExecution(params))
            .await?;
        Ok(())
    }

    /// Cancel the active run without closing the session.
    ///
    /// Queued follow-ups are preserved and the handle remains usable for
    /// future prompts.
    pub async fn cancel(&self) -> Result<(), HarnessError> {
        self.client.cancel_run().await?;
        Ok(())
    }

    /// Close this session permanently, cancelling active work, stopping event
    /// forwarding, and releasing its scheduler slot.
    pub async fn close(&self) -> Result<(), HarnessError> {
        self.session_manager.close_session(self.session_id).await?;
        Ok(())
    }

    /// Answer a pending tool permission request.
    pub async fn resolve_permission(
        &self,
        id: PermissionId,
        decision: PermissionDecision,
    ) -> Result<(), HarnessError> {
        self.client.resolve_permission(id, decision).await?;
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEventEnvelope> {
        self.client.subscribe()
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        self.client.snapshot()
    }

    pub fn context_inspection(&self) -> ContextInspection {
        let context = self.client.snapshot().context;
        ContextInspection {
            generation: context.generation,
            estimated_tokens: context.estimated_tokens,
            checkpoint: context.checkpoint,
            covered_through: context.covered_through,
            pinned_items: context.pinned_items,
            last_compacted_at: context.last_compacted_at,
        }
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }
}
