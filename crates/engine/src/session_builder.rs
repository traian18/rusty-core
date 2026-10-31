//! Public session builder and live session handle.
//!
//! The builder only collects configuration; `assembly` holds the steps
//! [`SessionBuilder::start`] runs and `handle` the live session API.

mod assembly;
mod handle;

mod options;
mod toolsets;

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::backend::BroadcastEventSink;

use assembly::DiscoveredTools;

use rusty_protocol::ids::SessionId;
use rusty_protocol::tools::AgentToolset;
use rusty_runtime::session_client::SessionClient;
use rusty_runtime::session_manager::{SessionManager, SessionManagerError};
use rusty_runtime::session_runtime::SessionError;
use rusty_runtime::traits::{ExecutionBackend, ToolRegistry};
use rusty_runtime::{IntegrationError, IntegrationRegistry};

pub use handle::{ContextInspection, SessionHandle};
pub use rusty_skills::SkillsConfig;
pub use rusty_tool_mcp::{McpServerConfig, McpTransportConfig};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Errors raised while configuring or operating a session.
type ContextProviderFactory =
    Box<dyn FnOnce(Arc<dyn ExecutionBackend>) -> Arc<dyn rusty_context::ContextProvider> + Send>;

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error(
        "This integration executes tools outside the harness. Use a model API integration so skill permissions can be enforced."
    )]
    BackendManagedTools,
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
    #[error("MCP server '{0}' failed: {1}")]
    Mcp(String, #[source] rusty_tool_mcp::McpError),
    #[error("session error: {0}")]
    Session(#[from] SessionError),
    #[error("session manager error: {0}")]
    SessionManager(#[from] SessionManagerError),
    #[error("session store error: {0}")]
    Store(#[from] rusty_session_store::StoreError),
    #[error("session {0} is not active")]
    SessionUnavailable(SessionId),
    #[error("invalid orchestration definition: {0}")]
    OrchestrationDefinition(String),
    #[error("orchestration runtime error: {0}")]
    OrchestrationRuntime(String),
    #[error("orchestration is not configured; enable it with HarnessBuilder::orchestration or SessionBuilder::orchestration")]
    OrchestrationNotConfigured,
    #[error("behavior profile error: {0}")]
    Profile(String),
}

struct PendingIntegration {
    id: String,
    config: serde_json::Value,
}

/// Fluent builder for direct or registry-backed sessions.
pub struct SessionBuilder {
    execution_policy: Option<rusty_protocol::tools::ExecutionPolicy>,
    backend: Option<Arc<dyn ExecutionBackend>>,
    integration: Option<PendingIntegration>,
    integrations: Arc<IntegrationRegistry>,
    tool_registry: Option<Arc<dyn ToolRegistry>>,
    workspace: Option<Arc<dyn rusty_runtime::traits::Workspace>>,
    /// The toolset to inject into the root agent's capabilities.
    /// When set alongside `tool_registry`, the builder creates the registry
    /// from the toolset's enabled descriptors via `build_executor_for()`.
    root_toolset: Option<AgentToolset>,
    /// The shared [`SessionManager`] that owns all active sessions.
    /// When `None`, a default manager is created in [`start`](Self::start).
    session_manager: Option<Arc<SessionManager>>,
    /// Optional context assembly/compaction provider — see
    /// [`context_provider`](Self::context_provider).
    context_provider: Option<Arc<dyn rusty_context::ContextProvider>>,
    /// Like `context_provider`, but built from the session's resolved
    /// backend — see [`context_provider_with_backend`](Self::context_provider_with_backend).
    context_factory: Option<ContextProviderFactory>,
    /// Session-level default execution params (model, max_tokens,
    /// temperature, reasoning, ...) applied immediately after the session
    /// starts — see [`execution_params`](Self::execution_params).
    execution_params: Option<rusty_protocol::backend::ExecutionParams>,
    /// MCP servers to connect at [`start`](Self::start) — see
    /// [`mcp_server`](Self::mcp_server).
    mcp_servers: Vec<McpServerConfig>,
    optional_mcp_servers: std::collections::HashSet<String>,
    /// Skill directories to scan at [`start`](Self::start) — see
    /// [`skills`](Self::skills). `None` disables skills entirely.
    skills: Option<SkillsConfig>,
    /// Orchestration support for the started session — see
    /// [`orchestration`](Self::orchestration).
    orchestration: Option<crate::orchestration::OrchestrationConfig>,
    /// Host profile registry; defaults to the built-ins only.
    profiles: Option<crate::profiles::ProfilesConfig>,
    /// The profile the root agent starts with — see [`profile`](Self::profile).
    profile: Option<rusty_agent_core::behavior::ProfileRef>,
    /// Workspace root to load `.rusty/profiles/` from.
    workspace_profiles: Option<std::path::PathBuf>,
    child_behavior_resolver: Option<Arc<dyn rusty_agent_core::behavior::ChildBehaviorResolver>>,
    command_trust: crate::profiles::CommandTrust,
    allow_draft_profiles: bool,
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
            execution_policy: None,
            backend: None,
            integration: None,
            integrations,
            tool_registry: None,
            workspace: None,
            root_toolset: None,
            session_manager: None,
            context_provider: None,
            context_factory: None,
            execution_params: None,
            mcp_servers: Vec::new(),
            optional_mcp_servers: Default::default(),
            skills: None,
            orchestration: None,
            profiles: None,
            profile: None,
            workspace_profiles: None,
            child_behavior_resolver: None,
            command_trust: Default::default(),
            allow_draft_profiles: false,
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
            session_manager: Some(session_manager),
            ..Self::with_integrations(integrations)
        }
    }

    /// Resolve configuration and create the live session.
    ///
    /// Session creation is routed through the [`SessionManager`], which
    /// handles ID generation, runtime construction, and supervisor task
    /// spawning. The resulting `Arc<SessionRuntime>` is registered with
    /// the manager for centralized lifecycle control.
    pub async fn start(self) -> Result<SessionHandle, HarnessError> {
        // Resolve the behavior profile first so a bad reference or a broken
        // workspace profile fails before MCP servers or backends are started.
        let (profile_registry, root_profile) = assembly::resolve_profiles(
            self.profiles.as_ref(),
            self.allow_draft_profiles,
            self.workspace_profiles.as_deref(),
            self.profile.as_ref(),
            self.command_trust,
        )?;
        let (backend, persisted_selection) =
            assembly::resolve_backend(self.backend, self.integration, &self.integrations).await?;
        let tool_registry = self
            .tool_registry
            .ok_or(HarnessError::MissingToolRegistry)?;

        if backend.capabilities().backend_managed_tools {
            return Err(HarnessError::BackendManagedTools);
        }

        // Connect any configured MCP servers and register their tools into
        // the same registry as the built-in ones — from here on an MCP
        // tool and `fs.read` look identical to the rest of the builder.
        // This needs to happen before `root_toolset` is derived below so
        // the no-explicit-toolset fallback (which reads straight from
        // `tool_registry.descriptors()`) picks the MCP tools up too.
        // Skills are discovered at the same point and for the same reason:
        // `skill.load`/`skill.read` have to be in the registry and in the
        // root toolset before either is frozen below.
        let mut discovered = DiscoveredTools::default();
        assembly::connect_mcp_servers(
            &self.mcp_servers,
            &self.optional_mcp_servers,
            self.execution_policy.as_ref(),
            tool_registry.as_ref(),
            &mut discovered,
        )
        .await?;
        let skills_provider = assembly::discover_skills(
            self.skills.as_ref(),
            tool_registry.as_ref(),
            &mut discovered,
        )
        .await;

        let root_toolset = assembly::root_toolset(
            self.root_toolset,
            discovered,
            tool_registry.as_ref(),
            self.execution_policy.as_ref(),
        );
        let backend = assembly::advertise_tools(backend, &root_toolset, persisted_selection);
        // An unbound session used to fall back to `FakeWorkspace`, an
        // in-memory store: `fs.read` reported "not found" for files that
        // exist on disk and `fs.edit` wrote into a buffer that was dropped
        // when the session ended — silently, with a successful tool result
        // either way. `UnboundWorkspace` keeps the no-tools case working
        // (nothing calls it) while turning the tools case into an
        // actionable error at the first call.
        let workspace: Arc<dyn rusty_runtime::traits::Workspace> = self
            .workspace
            .unwrap_or_else(|| Arc::new(rusty_workspace::UnboundWorkspace::new()));
        let backend = assembly::assemble_context(
            backend,
            &workspace,
            skills_provider,
            self.context_provider,
            self.context_factory,
        );

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
        if let Some(resolver) = self.child_behavior_resolver {
            runtime.set_child_behavior_resolver(resolver);
        }
        let session_id = runtime.session_id;

        let client = SessionClient::new(runtime);
        assembly::apply_session_defaults(&client, self.execution_params, &root_profile).await?;

        Ok(SessionHandle::new(
            client,
            session_id,
            session_manager,
            self.orchestration,
            Arc::new(profile_registry),
            self.command_trust,
        ))
    }
}

impl Default for SessionBuilder {
    fn default() -> Self {
        Self::new()
    }
}
