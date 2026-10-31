//! Bringing a stored session back, with the tool set the host supplies.

use super::*;
use std::sync::Arc;

use rusty_protocol::ids::SessionId;
use rusty_protocol::tools::AgentToolset;

use rusty_runtime::traits::{SimpleToolRegistry, ToolRegistry, Workspace};
use rusty_runtime::workspace::FakeWorkspace;

use crate::session_builder::{HarnessError, SessionHandle};
use crate::tool_factory::build_executor_with_context;
use rusty_code_analysis::CallGraph;
use rusty_tool_repo::RepositoryIntelligence;

impl Harness {
    /// Restores a session with the host's real workspace and tool policy.
    ///
    /// RC-304: restore is **strict** — the snapshot's recorded workspace
    /// identity and integration references are validated against the live
    /// `workspace` and the harness's integration registry, and a mismatch or
    /// missing provider rejects the restore instead of silently substituting
    /// a fake.
    pub async fn restore_session_with_toolset(
        &self,
        id: SessionId,
        toolset: AgentToolset,
        workspace: Arc<dyn Workspace>,
    ) -> Result<SessionHandle, HarnessError> {
        self.restore_session_with_tools(id, toolset, workspace, None, None)
            .await
    }

    /// Restore a session with explicitly authorized repository intelligence.
    pub async fn restore_session_with_toolset_and_repository_intelligence(
        &self,
        id: SessionId,
        toolset: AgentToolset,
        workspace: Arc<dyn Workspace>,
        intelligence: RepositoryIntelligence,
    ) -> Result<SessionHandle, HarnessError> {
        self.restore_session_with_tools(id, toolset, workspace, None, Some(intelligence))
            .await
    }

    /// Restore a session with an explicitly authorized shared call graph.
    pub async fn restore_session_with_toolset_and_call_graph(
        &self,
        id: SessionId,
        toolset: AgentToolset,
        workspace: Arc<dyn Workspace>,
        graph: Arc<dyn CallGraph>,
    ) -> Result<SessionHandle, HarnessError> {
        self.restore_session_with_tools(id, toolset, workspace, Some(graph), None)
            .await
    }

    pub(super) async fn restore_session_with_tools(
        &self,
        id: SessionId,
        toolset: AgentToolset,
        workspace: Arc<dyn Workspace>,
        graph: Option<Arc<dyn CallGraph>>,
        repository: Option<RepositoryIntelligence>,
    ) -> Result<SessionHandle, HarnessError> {
        let registry = SimpleToolRegistry::new();
        for descriptor in toolset.enabled_descriptors() {
            let executor = build_executor_with_context(
                descriptor,
                workspace.clone(),
                graph.clone(),
                repository.as_ref(),
            );
            let _ = registry.register(executor);
        }
        let tool_registry: Arc<dyn ToolRegistry> = Arc::new(registry);
        let runtime = self
            .sessions
            .restore_session(
                id,
                self.integrations.clone(),
                tool_registry,
                workspace,
                Arc::new(NoopEventSink),
            )
            .await?;
        Ok(SessionHandle::from_runtime(
            runtime,
            self.sessions.clone(),
            self.orchestration.clone(),
            Arc::new(self.profiles.snapshot()),
        ))
    }

    /// Restore a previously persisted session, returning a live
    /// [`SessionHandle`].
    ///
    /// The harness's integration registry re-creates the stored agents'
    /// execution backends, and the configured [`SessionStore`] supplies the
    /// durable snapshot/event history.
    ///
    /// # RC-300 restore contract
    ///
    /// Restore is **strict** (RC-304): the snapshot's recorded workspace
    /// identity and integration references are validated against the current
    /// host before any backend is created, and a missing dependency rejects
    /// the restore. This method binds an in-memory fake workspace, so it only
    /// succeeds when the snapshot recorded no workspace binding or one that
    /// matches the fake — for sessions created against a real workspace use
    /// [`restore_session_with_toolset`](Self::restore_session_with_toolset),
    /// which validates against the host's actual workspace.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::SessionManager`] when no store is configured,
    /// the session is not found, replay validation fails, dependency
    /// resolution rejects the restore, the stored session has no snapshot
    /// checkpoint, or a stored backend cannot be re-created.
    pub async fn restore_session(&self, id: SessionId) -> Result<SessionHandle, HarnessError> {
        let runtime = self
            .sessions
            .restore_session(
                id,
                self.integrations.clone(),
                Arc::new(SimpleToolRegistry::new()),
                Arc::new(FakeWorkspace::new()),
                Arc::new(NoopEventSink),
            )
            .await?;
        Ok(SessionHandle::from_runtime(
            runtime,
            self.sessions.clone(),
            self.orchestration.clone(),
            Arc::new(self.profiles.snapshot()),
        ))
    }
}
