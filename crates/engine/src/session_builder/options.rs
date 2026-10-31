//! The settings of a session before it starts: backend, tools, profiles, MCP servers, orchestration, skills and execution.

use super::*;
use serde::Serialize;
use std::sync::Arc;

use rusty_runtime::traits::{ExecutionBackend, ToolRegistry};

impl SessionBuilder {
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
    pub fn execution_policy(mut self, policy: rusty_protocol::tools::ExecutionPolicy) -> Self {
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
    pub fn profile(mut self, reference: rusty_agent_core::behavior::ProfileRef) -> Self {
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
        resolver: Arc<dyn rusty_agent_core::behavior::ChildBehaviorResolver>,
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
    pub fn workspace(mut self, workspace: Arc<dyn rusty_runtime::traits::Workspace>) -> Self {
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
    pub fn execution_params(mut self, params: rusty_protocol::backend::ExecutionParams) -> Self {
        self.execution_params = Some(params);
        self
    }
}
