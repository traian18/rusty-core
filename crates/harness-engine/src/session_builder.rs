//! Public session builder and live session handle.

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::broadcast;

use crate::backend::{discovered_capability, BroadcastEventSink, ToolAdvertisingBackend};
use crate::tool_factory::build_executor_for;
use harness_protocol::commands::{PermissionDecision, UserInput};
use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::{PermissionId, SessionId};
use harness_protocol::tools::AgentToolset;
use harness_runtime::session_client::{SessionClient, SessionSnapshot};
use harness_runtime::session_manager::{SessionManager, SessionManagerError};
use harness_runtime::session_runtime::{SessionCommand, SessionError, SessionRuntime};
use harness_runtime::traits::{ExecutionBackend, ToolRegistry};
use harness_runtime::{IntegrationError, IntegrationRegistry};

pub use harness_skills::SkillsConfig;
use harness_skills::{SkillCatalog, SkillsContextProvider};
pub use harness_tool_mcp::{McpServerConfig, McpTransportConfig};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Errors raised while configuring or operating a session.
type ContextProviderFactory =
    Box<dyn FnOnce(Arc<dyn ExecutionBackend>) -> Arc<dyn harness_context::ContextProvider> + Send>;

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
    Mcp(String, #[source] harness_tool_mcp::McpError),
    #[error("session error: {0}")]
    Session(#[from] SessionError),
    #[error("session manager error: {0}")]
    SessionManager(#[from] SessionManagerError),
    #[error("session store error: {0}")]
    Store(#[from] harness_session_store::StoreError),
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
    execution_policy: Option<harness_protocol::tools::ExecutionPolicy>,
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
    /// Like `context_provider`, but built from the session's resolved
    /// backend — see [`context_provider_with_backend`](Self::context_provider_with_backend).
    context_factory: Option<ContextProviderFactory>,
    /// Session-level default execution params (model, max_tokens,
    /// temperature, reasoning, ...) applied immediately after the session
    /// starts — see [`execution_params`](Self::execution_params).
    execution_params: Option<harness_protocol::backend::ExecutionParams>,
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
    profile: Option<harness_core::behavior::ProfileRef>,
    /// Workspace root to load `.rusty/profiles/` from.
    workspace_profiles: Option<std::path::PathBuf>,
    child_behavior_resolver: Option<Arc<dyn harness_core::behavior::ChildBehaviorResolver>>,
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
            execution_policy: None,
            backend: None,
            integration: None,
            integrations,
            tool_registry: None,
            workspace: None,
            root_toolset: None,
            session_manager: Some(session_manager),
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

    /// Enforce application mode and skill permissions after all tool discovery.
    pub fn execution_policy(mut self, policy: harness_protocol::tools::ExecutionPolicy) -> Self {
        self.execution_policy = Some(policy);
        self
    }

    /// Connect an MCP server at [`start`](Self::start) and register every
    /// tool it advertises alongside the session's other tools.
    ///
    /// Resolution is deferred to `start()` for the same reason
    /// [`integration`](Self::integration) is: connecting means spawning a
    /// process and awaiting its `initialize` handshake, which needs an
    /// async context the fluent builder doesn't have. Call this once per
    /// server; each gets its own process and its tools are namespaced
    /// `mcp.<name>.<tool>` so servers (and built-in tools) never collide.
    pub fn mcp_server(mut self, config: McpServerConfig) -> Self {
        self.mcp_servers.push(config);
        self
    }

    /// Like `mcp_server`, but an unavailable server does not abort the session.
    /// Permission checks still run before attempting connection.
    pub fn optional_mcp_server(mut self, config: McpServerConfig) -> Self {
        self.optional_mcp_servers.insert(config.name.clone());
        self.mcp_servers.push(config);
        self
    }

    /// Use `config`'s profile registry. Sessions created through a
    /// [`Harness`](crate::Harness) get the harness's registry automatically.
    pub fn profiles(mut self, config: crate::profiles::ProfilesConfig) -> Self {
        self.profiles = Some(config);
        self
    }

    /// Start the root agent under this behavior profile. Without it the
    /// workspace default applies (see [`workspace_profiles`](Self::workspace_profiles)),
    /// then the built-in `rusty.default`. An unresolvable reference fails
    /// [`start`](Self::start) before any side effect.
    pub fn profile(mut self, reference: harness_core::behavior::ProfileRef) -> Self {
        self.profile = Some(reference);
        self
    }

    /// Load profiles from `<root>/.rusty/profiles/` (and its `config.json`
    /// default). Opt-in, like skills, because workspace content is only
    /// trusted when the host says so.
    pub fn workspace_profiles(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.workspace_profiles = Some(root.into());
        self
    }

    /// Decide how spawned child agents get their behavior. Defaults to the
    /// parent profile's `children` policy (inherit).
    pub fn child_behavior_resolver(
        mut self,
        resolver: Arc<dyn harness_core::behavior::ChildBehaviorResolver>,
    ) -> Self {
        self.child_behavior_resolver = Some(resolver);
        self
    }

    /// Let behavior profiles run shell commands as completion-gate checks
    /// (`command` evaluators, compatible with Claude Code / Codex `Stop`
    /// hooks). Off by default: commands run arbitrary processes in the
    /// workspace.
    pub fn allow_command_evaluators(mut self, allow: bool) -> Self {
        self.command_trust.allowed = allow;
        self
    }

    /// Also let profiles loaded from the workspace (`.rusty/profiles/`) run
    /// commands. Requires [`allow_command_evaluators`](Self::allow_command_evaluators).
    pub fn trust_workspace_commands(mut self, trust: bool) -> Self {
        self.command_trust.trust_workspace = trust;
        self
    }

    /// Let draft profiles run: exact draft revisions resolve, and a
    /// reference without a revision picks the newest draft too. For editors
    /// and development, where profiles are tried before they are published.
    pub fn allow_draft_profiles(mut self, allow: bool) -> Self {
        self.allow_draft_profiles = allow;
        self
    }

    /// Enable orchestrated workflows on the started session (see
    /// [`SessionHandle::start_orchestration`]). Sessions created through a
    /// [`Harness`](crate::Harness) inherit its configuration.
    pub fn orchestration(mut self, config: crate::orchestration::OrchestrationConfig) -> Self {
        self.orchestration = Some(config);
        self
    }

    /// Discover filesystem skills at [`start`](Self::start), registering
    /// `skill.load`/`skill.read` and adding the skill catalog to the system
    /// prompt.
    ///
    /// Resolution is deferred to `start()` for the same reason
    /// [`mcp_server`](Self::mcp_server) is: discovery walks directories,
    /// which needs an async context the fluent builder doesn't have.
    ///
    /// Only each skill's **name and description** reach the system prompt;
    /// instruction bodies stay on disk until the model calls `skill.load`.
    /// A session with no discoverable skills pays nothing — no prompt text,
    /// and the tools are still registered so the model can be told about
    /// skills added later in the same workspace.
    ///
    /// Composes with [`context_provider`](Self::context_provider) rather
    /// than replacing it: the skills provider runs first, so a compaction
    /// provider set by the caller sizes its budget against the prompt that
    /// actually ships.
    pub fn skills(mut self, config: SkillsConfig) -> Self {
        self.skills = Some(config);
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

    /// Install a context provider that needs the session's own backend, such
    /// as a summarizer making model calls on the same provider. `factory` runs
    /// once in [`start`](Self::start) with the resolved backend (before any
    /// context wrapping, so its calls are not themselves compacted) and its
    /// provider runs after any provider set with
    /// [`context_provider`](Self::context_provider).
    pub fn context_provider_with_backend(
        mut self,
        factory: impl FnOnce(Arc<dyn ExecutionBackend>) -> Arc<dyn harness_context::ContextProvider>
            + Send
            + 'static,
    ) -> Self {
        self.context_factory = Some(Box::new(factory));
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
            let executor = build_executor_for(descriptor, workspace.clone());
            let _ = registry.register(executor);
        }

        // 2. Wire the registry + toolset into the root Agent's capabilities.
        self.tool_registry = Some(Arc::new(registry));
        self.root_toolset = Some(toolset);
        self.workspace = Some(workspace);
        self
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
        let mut profile_registry = self
            .profiles
            .as_ref()
            .map(crate::profiles::ProfilesConfig::snapshot)
            .unwrap_or_default()
            .allow_drafts(self.allow_draft_profiles);
        if let Some(root) = &self.workspace_profiles {
            crate::profiles::load_workspace_profiles(root, &mut profile_registry)?;
        }
        let root_profile = crate::profiles::resolve_bundle(
            &profile_registry,
            self.profile.as_ref(),
            self.command_trust,
        )?;

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

        if backend.capabilities().backend_managed_tools {
            return Err(HarnessError::BackendManagedTools);
        }

        // Connect any configured MCP servers and register their tools into
        // the same registry as the built-in ones — from here on an MCP
        // tool and `fs.read` look identical to the rest of the builder.
        // This needs to happen before `root_toolset` is derived below so
        // the no-explicit-toolset fallback (which reads straight from
        // `tool_registry.descriptors()`) picks the MCP tools up too.
        let mut discovered_descriptors = Vec::new();
        let mut allowed_mcp_tools = Vec::new();
        for config in &self.mcp_servers {
            use harness_core::execution_policy::{mcp_server_access, McpServerAccess};
            let access = self
                .execution_policy
                .as_ref()
                .map_or(McpServerAccess::Full, |policy| {
                    mcp_server_access(policy, &config.name)
                });
            let discovered = match access {
                McpServerAccess::Denied => continue,
                McpServerAccess::ReadOnly => {
                    harness_tool_mcp::connect_and_discover_read_only(config).await
                }
                McpServerAccess::Full => harness_tool_mcp::connect_and_discover(config).await,
            };
            let executors = match discovered {
                Ok(executors) => executors,
                Err(error) if self.optional_mcp_servers.contains(&config.name) => {
                    tracing::warn!(server = %config.name, %error, "skipping unavailable MCP server");
                    continue;
                }
                Err(error) => return Err(HarnessError::Mcp(config.name.clone(), error)),
            };
            for executor in executors {
                let descriptor = executor.descriptor();
                allowed_mcp_tools.push(descriptor.id.to_string());
                let _ = tool_registry.register(executor);
                discovered_descriptors.push(discovered_capability(descriptor));
            }
        }

        // Skills are discovered at the same point and for the same reason:
        // `skill.load`/`skill.read` have to be in the registry and in the
        // root toolset before either is frozen below.
        //
        // Unlike an MCP server, a broken skill is never fatal — one
        // malformed `SKILL.md` costs its author that skill and nothing
        // else, so `discover` hands back problems alongside the catalog and
        // we log them rather than failing the session.
        let skills_provider = match &self.skills {
            Some(config) => {
                let (catalog, errors) = SkillCatalog::discover(config).await;
                for error in &errors {
                    tracing::warn!(error = %error, "skills: skipping unreadable skill");
                }
                let catalog = Arc::new(catalog);
                tracing::debug!(skills = catalog.len(), "skills: catalog ready");

                for executor in harness_tool_skills::skill_tools(catalog.clone()) {
                    let descriptor = executor.descriptor();
                    let _ = tool_registry.register(executor);
                    discovered_descriptors.push(discovered_capability(descriptor));
                }
                Some(Arc::new(SkillsContextProvider::new(catalog))
                    as Arc<dyn harness_context::ContextProvider>)
            }
            None => None,
        };

        // A high-level toolset carries explicit policy. For callers that
        // provide a registry directly, derive an enabled/allowed toolset so
        // registered tools are also executable by the root agent.
        let mut root_toolset = match self.root_toolset {
            Some(mut toolset) => {
                // `.toolset()` already fixed its policy before MCP servers
                // and skills were discovered — merge the newly discovered
                // tools in rather than rebuilding from the registry, which
                // would silently drop the caller's explicit choices.
                for (id, capability) in discovered_descriptors {
                    if !toolset
                        .tools
                        .values()
                        .any(|existing| existing.descriptor.name == capability.descriptor.name)
                    {
                        toolset.tools.insert(id, capability);
                    }
                }
                toolset
            }
            None => {
                let tools = tool_registry
                    .descriptors()
                    .into_iter()
                    .map(discovered_capability)
                    .collect();
                AgentToolset { tools }
            }
        };
        if let Some(policy) = &self.execution_policy {
            harness_core::execution_policy::restrict_toolset(
                policy,
                &mut root_toolset,
                &allowed_mcp_tools,
            );
        }
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
        // An unbound session used to fall back to `FakeWorkspace`, an
        // in-memory store: `fs.read` reported "not found" for files that
        // exist on disk and `fs.edit` wrote into a buffer that was dropped
        // when the session ended — silently, with a successful tool result
        // either way. `UnboundWorkspace` keeps the no-tools case working
        // (nothing calls it) while turning the tools case into an
        // actionable error at the first call.
        let workspace: Arc<dyn harness_runtime::traits::Workspace> = self
            .workspace
            .unwrap_or_else(|| Arc::new(harness_workspace::UnboundWorkspace::new()));

        // The skills provider runs *before* whatever the caller installed,
        // so that a compaction provider (which sizes a token budget) sees
        // the prompt that will actually ship rather than one still missing
        // the skill catalog.
        let factory_provider = self.context_factory.map(|factory| factory(backend.clone()));
        let caller_provider = match (self.context_provider, factory_provider) {
            (Some(first), Some(second)) => {
                Some(Arc::new(harness_context::ChainedContextProvider::new(vec![
                    first, second,
                ]))
                    as Arc<dyn harness_context::ContextProvider>)
            }
            (only, None) | (None, only) => only,
        };
        let context_provider = match (skills_provider, caller_provider) {
            (Some(skills), Some(caller)) => {
                Some(Arc::new(harness_context::ChainedContextProvider::new(vec![
                    skills, caller,
                ]))
                    as Arc<dyn harness_context::ContextProvider>)
            }
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        };

        let backend: Arc<dyn ExecutionBackend> = match context_provider {
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
        if let Some(resolver) = self.child_behavior_resolver {
            runtime.set_child_behavior_resolver(resolver);
        }
        let session_id = runtime.session_id;

        let client = SessionClient::new(runtime);
        if let Some(params) = self.execution_params {
            client
                .send(SessionCommand::ConfigureExecution(params))
                .await?;
        }
        if !root_profile.is_default() {
            client.send(root_profile.command()).await?;
        }

        Ok(SessionHandle {
            client,
            session_id,
            session_manager,
            orchestration: self.orchestration,
            profiles: Arc::new(profile_registry),
            command_trust: self.command_trust,
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
    pub(crate) session_id: SessionId,
    pub(crate) session_manager: Arc<SessionManager>,
    pub(crate) orchestration: Option<crate::orchestration::OrchestrationConfig>,
    /// Profiles this session resolves references against (host registry
    /// plus any workspace profiles loaded at start).
    pub(crate) profiles: Arc<harness_core::behavior::ProfileRegistry>,
    /// Whether this session's profiles may run commands.
    pub(crate) command_trust: crate::profiles::CommandTrust,
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
        orchestration: Option<crate::orchestration::OrchestrationConfig>,
        profiles: Arc<harness_core::behavior::ProfileRegistry>,
    ) -> Self {
        let session_id = runtime.session_id;
        // A restored agent keeps the trust it was installed with; new
        // installs through this handle get none unless re-granted.
        Self {
            client: SessionClient::new(runtime),
            session_id,
            session_manager,
            orchestration,
            profiles,
            command_trust: Default::default(),
        }
    }

    /// The behavior profile the root agent is running under.
    pub async fn behavior_profile(
        &self,
    ) -> Result<harness_core::behavior::ProfileRef, HarnessError> {
        let runtime = self
            .session_manager
            .session_handle(self.session_id)
            .await
            .ok_or(HarnessError::SessionUnavailable(self.session_id))?;
        Ok(runtime.root_behavior().reference())
    }

    /// Switch the root agent to another registered profile (e.g. plan →
    /// build). Takes effect immediately when the agent is idle; during a run
    /// it applies before the next model request, and the model is told its
    /// mode changed.
    pub async fn set_behavior_profile(
        &self,
        reference: harness_core::behavior::ProfileRef,
    ) -> Result<(), HarnessError> {
        let bundle =
            crate::profiles::resolve_bundle(&self.profiles, Some(&reference), self.command_trust)?;
        self.client.send(bundle.command()).await?;
        Ok(())
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
    /// effect starting with the root agent's next model request -- the next
    /// prompt/steer/follow-up, or the active run's next turn -- and never
    /// mutates an already-in-flight request. Use this both to change the session's
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

    /// Change the execution parameters (e.g. the model) of a session this one
    /// delegates to -- an orchestration step running right now -- from its
    /// next model request. The step keeps the output format it was started
    /// with unless `params` sets one.
    pub async fn set_delegated_execution_params(
        &self,
        session_id: SessionId,
        mut params: harness_protocol::backend::ExecutionParams,
    ) -> Result<(), HarnessError> {
        let parent = self
            .session_manager
            .session_handle(self.session_id)
            .await
            .ok_or(HarnessError::SessionUnavailable(self.session_id))?;
        let step = parent
            .delegated_session(session_id)
            .ok_or(HarnessError::SessionUnavailable(session_id))?;
        if params.response_format.is_none() {
            params.response_format = step.root_execution_params().response_format;
        }
        step.send_command(SessionCommand::ConfigureExecution(params))
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

    /// Pause the session's active run.
    ///
    /// Unlike [`cancel`](Self::cancel) this is recoverable: the agent stops
    /// at its next transition point and holds its run state, so
    /// [`resume`](Self::resume) continues the same run rather than starting a
    /// new one. Pausing an already-paused, cancelled, or failed agent is a
    /// no-op — the state machine rejects the transition and emits nothing.
    ///
    /// Note that this does not abort an in-flight backend request; a run
    /// waiting on the model pauses once that request settles.
    pub async fn pause(&self) -> Result<(), HarnessError> {
        self.client.send(SessionCommand::Pause).await?;
        Ok(())
    }

    /// Resume a paused session, continuing the run that was interrupted.
    ///
    /// A no-op unless the agent is actually paused.
    pub async fn resume(&self) -> Result<(), HarnessError> {
        self.client.send(SessionCommand::Resume).await?;
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
