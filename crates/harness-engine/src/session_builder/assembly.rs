//! The steps [`SessionBuilder::start`](super::SessionBuilder::start) runs, in
//! order, to turn a builder's configuration into a live session.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use harness_context::ContextProvider;
use harness_core::behavior::{ProfileRef, ProfileRegistry};
use harness_core::execution_policy::{mcp_server_access, McpServerAccess};
use harness_protocol::backend::{ExecutionParams, PersistedBackendSelection};
use harness_protocol::ids::ToolId;
use harness_protocol::tools::{AgentToolset, ExecutionPolicy, ToolCapability};
use harness_runtime::session_client::SessionClient;
use harness_runtime::session_runtime::SessionCommand;
use harness_runtime::traits::{ExecutionBackend, ToolRegistry, Workspace};
use harness_runtime::IntegrationRegistry;
use harness_skills::{SkillCatalog, SkillsConfig, SkillsContextProvider};
use harness_tool_mcp::McpServerConfig;
use harness_tools::{ToolDescriptor, ToolExecutor};

use super::{ContextProviderFactory, HarnessError, PendingIntegration};
use crate::backend::{discovered_capability, ToolAdvertisingBackend};
use crate::profiles::{CommandTrust, ProfileBundle, ProfilesConfig};

/// Tools found while starting, beyond those the caller registered up front.
#[derive(Default)]
pub(super) struct DiscoveredTools {
    capabilities: Vec<(ToolId, ToolCapability)>,
    /// MCP tool ids the execution policy has to leave allowed.
    mcp_tool_ids: Vec<String>,
}

/// Build the profile registry (host profiles plus any workspace ones) and
/// resolve the profile the root agent starts with.
pub(super) fn resolve_profiles(
    profiles: Option<&ProfilesConfig>,
    allow_drafts: bool,
    workspace_root: Option<&Path>,
    profile: Option<&ProfileRef>,
    trust: CommandTrust,
) -> Result<(ProfileRegistry, ProfileBundle), HarnessError> {
    let mut registry = profiles
        .map(ProfilesConfig::snapshot)
        .unwrap_or_default()
        .allow_drafts(allow_drafts);
    if let Some(root) = workspace_root {
        crate::profiles::load_workspace_profiles(root, &mut registry)?;
    }
    let bundle = crate::profiles::resolve_bundle(&registry, profile, trust)?;
    Ok((registry, bundle))
}

/// Use the injected backend, or create one from the selected integration.
pub(super) async fn resolve_backend(
    backend: Option<Arc<dyn ExecutionBackend>>,
    integration: Option<PendingIntegration>,
    integrations: &IntegrationRegistry,
) -> Result<(Arc<dyn ExecutionBackend>, Option<PersistedBackendSelection>), HarnessError> {
    if let Some(backend) = backend {
        return Ok((backend, None));
    }
    let integration = integration.ok_or(HarnessError::MissingBackend)?;
    let selection = integration
        .config
        .get("_backend_selection")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    let backend = integrations
        .create(&integration.id, integration.config)
        .await?;
    Ok((backend, selection))
}

/// Register `executor` and return its descriptor. A clash leaves the tool
/// already registered under that id in place; it is logged, not fatal.
pub(super) fn register_tool(
    registry: &dyn ToolRegistry,
    executor: Arc<dyn ToolExecutor>,
) -> ToolDescriptor {
    let descriptor = executor.descriptor();
    if let Err(error) = registry.register(executor) {
        tracing::warn!(tool = %descriptor.id, %error, "tool not registered");
    }
    descriptor
}

/// Connect every MCP server the execution policy allows and register the
/// tools each advertises.
pub(super) async fn connect_mcp_servers(
    servers: &[McpServerConfig],
    optional: &HashSet<String>,
    policy: Option<&ExecutionPolicy>,
    registry: &dyn ToolRegistry,
    discovered: &mut DiscoveredTools,
) -> Result<(), HarnessError> {
    for config in servers {
        let is_optional = optional.contains(&config.name);
        let Some(executors) = discover_mcp_server(config, policy, is_optional).await? else {
            continue;
        };
        for executor in executors {
            let descriptor = register_tool(registry, executor);
            discovered.mcp_tool_ids.push(descriptor.id.to_string());
            discovered
                .capabilities
                .push(discovered_capability(descriptor));
        }
    }
    Ok(())
}

/// Connect one server as far as the policy allows. `None` when the policy
/// denies it, or when an optional server is unavailable.
async fn discover_mcp_server(
    config: &McpServerConfig,
    policy: Option<&ExecutionPolicy>,
    optional: bool,
) -> Result<Option<Vec<Arc<dyn ToolExecutor>>>, HarnessError> {
    let access = policy.map_or(McpServerAccess::Full, |policy| {
        mcp_server_access(policy, &config.name)
    });
    let discovered = match access {
        McpServerAccess::Denied => return Ok(None),
        McpServerAccess::ReadOnly => harness_tool_mcp::connect_and_discover_read_only(config).await,
        McpServerAccess::Full => harness_tool_mcp::connect_and_discover(config).await,
    };
    match discovered {
        Ok(executors) => Ok(Some(executors)),
        Err(error) if optional => {
            tracing::warn!(server = %config.name, %error, "skipping unavailable MCP server");
            Ok(None)
        }
        Err(error) => Err(HarnessError::Mcp(config.name.clone(), error)),
    }
}

/// Discover skills, register `skill.load`/`skill.read`, and return the
/// provider that adds the skill catalog to the system prompt.
///
/// Unlike an MCP server, a broken skill is never fatal — one malformed
/// `SKILL.md` costs its author that skill and nothing else, so `discover`
/// hands back problems alongside the catalog and we log them rather than
/// failing the session.
pub(super) async fn discover_skills(
    config: Option<&SkillsConfig>,
    registry: &dyn ToolRegistry,
    discovered: &mut DiscoveredTools,
) -> Option<Arc<dyn ContextProvider>> {
    let (catalog, errors) = SkillCatalog::discover(config?).await;
    for error in &errors {
        tracing::warn!(error = %error, "skills: skipping unreadable skill");
    }
    let catalog = Arc::new(catalog);
    tracing::debug!(skills = catalog.len(), "skills: catalog ready");

    for executor in harness_tool_skills::skill_tools(catalog.clone()) {
        let descriptor = register_tool(registry, executor);
        discovered
            .capabilities
            .push(discovered_capability(descriptor));
    }
    Some(Arc::new(SkillsContextProvider::new(catalog)))
}

/// The root agent's toolset, restricted by the execution policy.
///
/// A high-level toolset carries explicit policy. For callers that provide a
/// registry directly, derive an enabled/allowed toolset so registered tools
/// are also executable by the root agent.
pub(super) fn root_toolset(
    explicit: Option<AgentToolset>,
    discovered: DiscoveredTools,
    registry: &dyn ToolRegistry,
    policy: Option<&ExecutionPolicy>,
) -> AgentToolset {
    let mut toolset = match explicit {
        Some(toolset) => merge_discovered(toolset, discovered.capabilities),
        None => AgentToolset {
            tools: registry
                .descriptors()
                .into_iter()
                .map(discovered_capability)
                .collect(),
        },
    };
    if let Some(policy) = policy {
        harness_core::execution_policy::restrict_toolset(
            policy,
            &mut toolset,
            &discovered.mcp_tool_ids,
        );
    }
    toolset
}

/// `.toolset()` already fixed its policy before MCP servers and skills were
/// discovered — merge the newly discovered tools in rather than rebuilding
/// from the registry, which would silently drop the caller's explicit choices.
fn merge_discovered(
    mut toolset: AgentToolset,
    capabilities: Vec<(ToolId, ToolCapability)>,
) -> AgentToolset {
    for (id, capability) in capabilities {
        let known = toolset
            .tools
            .values()
            .any(|existing| existing.descriptor.name == capability.descriptor.name);
        if !known {
            toolset.tools.insert(id, capability);
        }
    }
    toolset
}

/// Wrap `backend` so every request advertises the toolset's enabled tools.
pub(super) fn advertise_tools(
    backend: Arc<dyn ExecutionBackend>,
    toolset: &AgentToolset,
    selection: Option<PersistedBackendSelection>,
) -> Arc<dyn ExecutionBackend> {
    Arc::new(ToolAdvertisingBackend {
        inner: backend,
        tools: toolset.enabled_descriptors().into_iter().cloned().collect(),
        selection,
    })
}

/// Wrap `backend` in the session's context providers, if it has any.
///
/// The skills provider runs *before* whatever the caller installed, so that
/// a compaction provider (which sizes a token budget) sees the prompt that
/// will actually ship rather than one still missing the skill catalog.
pub(super) fn assemble_context(
    backend: Arc<dyn ExecutionBackend>,
    workspace: &Arc<dyn Workspace>,
    skills: Option<Arc<dyn ContextProvider>>,
    caller: Option<Arc<dyn ContextProvider>>,
    factory: Option<ContextProviderFactory>,
) -> Arc<dyn ExecutionBackend> {
    let factory_provider = factory.map(|factory| factory(backend.clone()));
    let caller = chain(caller, factory_provider);
    match chain(skills, caller) {
        Some(provider) => Arc::new(harness_context::ContextAssemblingBackend::new(
            backend,
            provider,
            workspace.clone(),
        )),
        None => backend,
    }
}

/// Chain two optional providers, `first` running before `second`.
fn chain(
    first: Option<Arc<dyn ContextProvider>>,
    second: Option<Arc<dyn ContextProvider>>,
) -> Option<Arc<dyn ContextProvider>> {
    match (first, second) {
        (Some(first), Some(second)) => Some(Arc::new(
            harness_context::ChainedContextProvider::new(vec![first, second]),
        )),
        (only, None) | (None, only) => only,
    }
}

/// Apply the session defaults before the handle is returned, so the first
/// prompt already runs with them.
pub(super) async fn apply_session_defaults(
    client: &SessionClient,
    params: Option<ExecutionParams>,
    profile: &ProfileBundle,
) -> Result<(), HarnessError> {
    if let Some(params) = params {
        client
            .send(SessionCommand::ConfigureExecution(params))
            .await?;
    }
    if !profile.is_default() {
        client.send(profile.command()).await?;
    }
    Ok(())
}
