//! The children a supervisor knows about: registering them, waiting for them, cancelling them.

use std::sync::Arc;

use rusty_agent_core::capabilities::AgentCapabilities;
use rusty_protocol::commands::{AgentCommand, AgentResult};
use rusty_protocol::ids::AgentId;
use tokio::sync::mpsc;

use super::{AgentHandle, AgentSupervisor, SupervisorError};
use crate::traits::Workspace;

impl AgentSupervisor {
    pub(super) async fn register(&self, child_id: AgentId, handle: AgentHandle) {
        let mut agents = self.agents.write().await;
        let mut children = self.children_of.write().await;
        let parent_id = handle.parent_id;
        agents.insert(child_id, handle);
        if let Some(parent_id) = parent_id {
            children.entry(parent_id).or_default().push(child_id);
        }
    }

    /// Forgets a finished child that nobody will await.
    pub(super) async fn deregister(&self, child_id: AgentId) {
        let removed = self.agents.write().await.remove(&child_id);
        if let Some(parent_id) = removed.and_then(|handle| handle.parent_id) {
            self.forget_child_of(parent_id, child_id).await;
        }
        self.agent_tokens_mut().remove(&child_id);
    }

    async fn forget_child_of(&self, parent_id: AgentId, child_id: AgentId) {
        let mut children = self.children_of.write().await;
        if let Some(ids) = children.get_mut(&parent_id) {
            ids.retain(|id| *id != child_id);
        }
    }

    pub async fn await_child(&self, child_id: AgentId) -> Result<AgentResult, SupervisorError> {
        let handle = self
            .agents
            .write()
            .await
            .remove(&child_id)
            .ok_or(SupervisorError::ChildResultLost(child_id))?;
        if let Some(parent_id) = handle.parent_id {
            self.forget_child_of(parent_id, child_id).await;
        }
        let _ = handle.join.await;
        self.agent_tokens_mut().remove(&child_id);
        handle
            .result
            .await
            .map_err(|_| SupervisorError::ChildResultLost(child_id))
    }

    pub async fn child_capabilities(&self, child_id: AgentId) -> Option<AgentCapabilities> {
        self.child_capabilities.read().await.get(&child_id).cloned()
    }

    pub async fn child_workspace(&self, child_id: AgentId) -> Option<Arc<dyn Workspace>> {
        self.child_workspaces.read().await.get(&child_id).cloned()
    }

    pub async fn child_commands(&self, child_id: AgentId) -> Option<mpsc::Sender<AgentCommand>> {
        self.agents
            .read()
            .await
            .get(&child_id)
            .map(|handle| handle.commands.clone())
    }

    /// Cancels one child subtree without affecting its parent or siblings.
    pub async fn cancel_child(&self, child_id: AgentId) -> Result<(), SupervisorError> {
        let agents = self.agents.read().await;
        let handle = agents
            .get(&child_id)
            .ok_or(SupervisorError::ChildResultLost(child_id))?;
        handle.cancel.cancel();
        Ok(())
    }
}
