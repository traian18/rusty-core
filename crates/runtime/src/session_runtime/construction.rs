//! Fresh-session construction: builds a brand-new root agent and starts the session around it.
//! The restore-time counterpart lives in [`super::restoration`].

use std::collections::HashMap;
use std::sync::Arc;

use rusty_agent_core::agent::Agent;
use rusty_agent_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
use rusty_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference,
};
use rusty_protocol::ids::{AgentId, BackendId, ConfigurationId, IntegrationId, SessionId};
use rusty_protocol::tools::AgentToolset;
use rusty_protocol::usage::AgentBudget;

use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

use super::assembly::SessionHost;
use super::SessionRuntime;

impl SessionRuntime {
    /// Creates a session runtime with a default scheduler and a root agent with no tools.
    pub fn new(
        session_id: SessionId,
        backend: Arc<dyn ExecutionBackend>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
    ) -> Self {
        let toolset = AgentToolset {
            tools: HashMap::new(),
        };
        Self::new_with_toolset(
            session_id,
            backend,
            tool_registry,
            workspace,
            event_sink,
            toolset,
        )
    }

    /// Creates a session runtime whose root agent receives the supplied tool capabilities, using a
    /// default scheduler.
    pub fn new_with_toolset(
        session_id: SessionId,
        backend: Arc<dyn ExecutionBackend>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        root_toolset: AgentToolset,
    ) -> Self {
        let host = SessionHost::new(session_id, backend, tool_registry, workspace, event_sink);
        Self::start(host, root_toolset, None)
    }

    /// Starts a session on `host` with a fresh root agent that holds `root_toolset`.
    ///
    /// `backend_integration_id` is what makes a session restorable: the root agent's persisted
    /// `BackendBinding` records it, and restoring resolves a snapshot's integration references by
    /// looking that exact id up in the live `IntegrationRegistry`. A session created through a
    /// registered integration should pass the integration's stable id. Without one, a random id is
    /// minted that can never resolve, so such a session is honestly not restorable instead of
    /// silently reusing another session's factory.
    pub fn start(
        host: SessionHost,
        root_toolset: AgentToolset,
        backend_integration_id: Option<IntegrationId>,
    ) -> Self {
        let root = root_agent(
            &host,
            root_toolset,
            backend_integration_id.unwrap_or_default(),
        );
        Self::assemble(host, root, Vec::new())
    }
}

/// The session's root agent: it may spawn children, and may read, write and search where its
/// tools do.
fn root_agent(host: &SessionHost, toolset: AgentToolset, integration: IntegrationId) -> Agent {
    let workspace = workspace_capabilities(&toolset);
    Agent::new(
        AgentId::new(),
        host.session_id,
        None,
        0,
        String::new(),
        BackendBinding {
            reference: BackendReference {
                integration,
                configuration: ConfigurationId::new(),
                model: None,
            },
            descriptor: BackendDescriptor {
                id: BackendId::new(),
                name: host.backend.descriptor().name,
                description: host.backend.descriptor().description,
                capabilities: host.backend.capabilities(),
            },
        },
        AgentCapabilities {
            tools: toolset,
            can_spawn_agents: true,
            max_child_depth: Some(8),
            workspace,
            backend: BackendCapabilities::default(),
        },
        AgentBudget::default(),
    )
}

fn workspace_capabilities(toolset: &AgentToolset) -> WorkspaceCapabilities {
    let enabled: Vec<_> = toolset
        .enabled_descriptors()
        .into_iter()
        .map(|descriptor| descriptor.name.as_str())
        .collect();
    WorkspaceCapabilities {
        can_read: enabled.contains(&"fs.read"),
        can_write: enabled.contains(&"fs.edit"),
        can_search: enabled.contains(&"workspace.search"),
    }
}
