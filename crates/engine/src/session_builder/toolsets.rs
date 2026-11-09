//! The tool sets a session can be built with, and the context providers that wrap its backend.

use super::*;
use std::sync::Arc;

use crate::tool_factory::build_executor_with_context;

use rusty_code_analysis::CallGraph;

use rusty_protocol::tools::AgentToolset;

use rusty_runtime::traits::ExecutionBackend;

use rusty_tool_repo::RepositoryIntelligence;

impl SessionBuilder {
    /// Install a context-assembly/compaction provider.
    ///
    /// When set, [`start`](Self::start) wraps the resolved backend in a
    /// [`rusty_context::ContextAssemblingBackend`] — every request's
    /// `system_prompt`/`messages` are rewritten by `provider` before
    /// reaching the backend. Mirrors how [`toolset`](Self::toolset) wraps
    /// the backend in `ToolAdvertisingBackend`; see
    /// `crates/rusty-context/PLAN.md` for why this is a backend decorator
    /// rather than a change to `rusty-agent-core`.
    pub fn context_provider(mut self, provider: Arc<dyn rusty_context::ContextProvider>) -> Self {
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
        factory: impl FnOnce(Arc<dyn ExecutionBackend>) -> Arc<dyn rusty_context::ContextProvider>
            + Send
            + 'static,
    ) -> Self {
        self.context_factory = Some(Box::new(factory));
        self
    }

    /// The session's resolved model and its known context window. The context wrapper reports a
    /// request's estimated input budget only for requests on this model; without it every
    /// estimate is unavailable.
    pub fn trusted_model_window(mut self, model: impl Into<String>, model_window: u64) -> Self {
        self.trusted_model_window = Some(rusty_context::provider::TrustedModelWindow {
            model: model.into(),
            model_window,
        });
        self
    }

    /// Register an `AgentToolset` into the session.
    ///
    /// This is an alternative to [`tools`](Self::tools) that takes a
    /// high-level tool policy set and a [`Workspace`](rusty_runtime::traits::Workspace)
    /// reference, creates the appropriate [`ToolExecutor`](rusty_runtime::traits::ToolExecutor)
    /// instances via `build_executor_for`, registers them into a
    /// [`SimpleToolRegistry`](rusty_runtime::traits::SimpleToolRegistry), and wires both the
    /// registry and the toolset into the root agent's capabilities.
    ///
    /// When this method is used, the builder will automatically populate
    /// the tool registry — there is no need to also call [`tools`](Self::tools).
    ///
    /// The `workspace` argument provides the [`Workspace`](rusty_runtime::traits::Workspace)
    /// that filesystem tools (`fs.read`, `fs.edit`, `workspace.search`) will delegate to.
    /// Shell tools (`shell.exec`) ignore the workspace.
    pub fn toolset(
        self,
        toolset: AgentToolset,
        workspace: Arc<dyn rusty_runtime::traits::Workspace>,
    ) -> Self {
        self.toolset_with_tools(toolset, workspace, None, None)
    }

    /// Register a toolset with an explicitly authorized shared call graph.
    pub fn toolset_with_call_graph(
        self,
        toolset: AgentToolset,
        workspace: Arc<dyn rusty_runtime::traits::Workspace>,
        graph: Arc<dyn CallGraph>,
    ) -> Self {
        self.toolset_with_tools(toolset, workspace, Some(graph), None)
    }

    /// Register a toolset with explicitly authorized repository intelligence.
    ///
    /// `repo_graph.*` tools exist only when `intelligence` carries a graph, and `repo_git.*`
    /// tools only when it carries a Git analysis; any such tool in the toolset otherwise fails
    /// closed as an unknown tool. Share one `intelligence` value only within one authorized
    /// repository context.
    pub fn toolset_with_repository_intelligence(
        self,
        toolset: AgentToolset,
        workspace: Arc<dyn rusty_runtime::traits::Workspace>,
        intelligence: RepositoryIntelligence,
    ) -> Self {
        self.toolset_with_tools(toolset, workspace, None, Some(intelligence))
    }

    pub(super) fn toolset_with_tools(
        mut self,
        toolset: AgentToolset,
        workspace: Arc<dyn rusty_runtime::traits::Workspace>,
        graph: Option<Arc<dyn CallGraph>>,
        repository: Option<RepositoryIntelligence>,
    ) -> Self {
        let registry = rusty_runtime::traits::SimpleToolRegistry::new();
        for descriptor in toolset.enabled_descriptors() {
            let executor = build_executor_with_context(
                descriptor,
                workspace.clone(),
                graph.clone(),
                repository.as_ref(),
            );
            assembly::register_tool(&registry, executor);
        }

        self.tool_registry = Some(Arc::new(registry));
        self.root_toolset = Some(toolset);
        self.workspace = Some(workspace);
        self
    }
}
