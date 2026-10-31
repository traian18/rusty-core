//! What a child agent inherits from the session that spawns it.

use std::sync::Arc;

use rusty_agent_core::agent::Agent;
use rusty_protocol::commands::AgentCommand;
use tokio::sync::mpsc;

use crate::integration::IntegrationRegistry;
use crate::scheduler::Scheduler;
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

/// The parent agent and the host services its children are built from.
pub struct SpawnHost<'a> {
    pub parent: &'a Agent,
    /// The parent's backend, which a child inherits unless its spec names another.
    pub backend: Arc<dyn ExecutionBackend>,
    pub integrations: &'a IntegrationRegistry,
    pub tools: Arc<dyn ToolRegistry>,
    pub workspace: Arc<dyn Workspace>,
    pub events: Arc<dyn EventSink>,
    pub scheduler: Arc<Scheduler>,
    /// Where the parent hears that the child finished.
    pub parent_commands: mpsc::Sender<AgentCommand>,
}
