//! Hierarchical supervision and child-agent spawning for one session.
//!
//! The supervisor tracks every child of the session, enforces the tree's limits when one is
//! spawned (`spawn`, `budget`), and lets parents wait for or cancel their children (`children`).

mod budget;
mod children;
mod host;
mod spawn;

pub use host::SpawnHost;

use rusty_agent_core::behavior::{ChildBehaviorResolver, PolicyChildBehaviorResolver};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::RwLock as StdRwLock;

use rusty_agent_core::agent::Agent;
use rusty_agent_core::budget::{BudgetCheck, BudgetError};
use rusty_agent_core::capabilities::{AgentCapabilities, CapabilityError};
use rusty_protocol::commands::{AgentCommand, AgentResult};

use rusty_protocol::ids::{AgentId, SessionId};
use rusty_protocol::usage::AgentBudget;
use rusty_workspace::WorkspaceError;

use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::cancellation::SessionCancellation;
use crate::integration::IntegrationError;

use crate::traits::Workspace;

/// Per-child bookkeeping tracked by the supervisor.
///
/// `depth` records the child's nesting depth (used for `max_depth` accounting
/// on subsequent generations) and `cancel` is the child's dedicated
/// cancellation token (reserved for future per-child cancellation; today
/// cancellation is session-scoped via [`SessionCancellation`]).
#[allow(dead_code)]
pub(super) struct AgentHandle {
    pub(super) parent_id: Option<AgentId>,
    pub(super) depth: u32,
    pub(super) cancel: CancellationToken,
    pub(super) join: JoinHandle<()>,
    pub(super) commands: mpsc::Sender<AgentCommand>,
    pub(super) result: oneshot::Receiver<AgentResult>,
}

/// The result of spawning according to SpawnMode.
#[derive(Debug, Clone)]
pub enum SpawnOutcome {
    /// Boxed: a child's full result is far larger than a `Detached` id.
    Awaited(Box<AgentResult>),
    Detached(AgentId),
}

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("budget: {0}")]
    Budget(#[from] BudgetError),
    #[error("capability: {0}")]
    Capability(#[from] CapabilityError),
    #[error("integration: {0}")]
    Integration(#[from] IntegrationError),
    #[error("workspace: {0}")]
    Workspace(#[from] WorkspaceError),
    #[error("parent agent not found: {0:?}")]
    ParentNotFound(AgentId),
    #[error("agent {0:?} is not allowed to spawn children")]
    SpawnNotAllowed(AgentId),
    #[error("child budget is looser than its parent for {0}")]
    BudgetEscalation(&'static str),
    #[error("child {0:?} finished without producing a result")]
    ChildResultLost(AgentId),
    #[error("child behavior: {0}")]
    ChildBehavior(#[from] rusty_agent_core::behavior::ChildPolicyError),
}

#[derive(Clone)]
pub struct AgentSupervisor {
    /// The session this supervisor is scoped to.
    #[allow(dead_code)]
    session_id: SessionId,
    session_cancel: SessionCancellation,
    agents: Arc<RwLock<HashMap<AgentId, AgentHandle>>>,
    children_of: Arc<RwLock<HashMap<AgentId, Vec<AgentId>>>>,
    child_capabilities: Arc<RwLock<HashMap<AgentId, AgentCapabilities>>>,
    child_workspaces: Arc<RwLock<HashMap<AgentId, Arc<dyn Workspace>>>>,
    agent_tokens: Arc<StdRwLock<HashMap<AgentId, CancellationToken>>>,
    /// Decides the behavior profile each spawned child starts with. Shared
    /// by every clone of this supervisor, so replacing it applies session-wide.
    child_behavior: Arc<StdRwLock<Arc<dyn ChildBehaviorResolver>>>,
}

impl AgentSupervisor {
    pub fn new(session_id: SessionId, session_cancel: SessionCancellation) -> Self {
        Self {
            session_id,
            session_cancel,
            agents: Arc::new(RwLock::new(HashMap::new())),
            children_of: Arc::new(RwLock::new(HashMap::new())),
            child_capabilities: Arc::new(RwLock::new(HashMap::new())),
            child_workspaces: Arc::new(RwLock::new(HashMap::new())),
            agent_tokens: Arc::new(StdRwLock::new(HashMap::new())),
            child_behavior: Arc::new(StdRwLock::new(Arc::new(PolicyChildBehaviorResolver))),
        }
    }

    /// Replace how children get their behavior profile (default: follow the
    /// parent profile's `children` policy, i.e. inherit).
    pub fn set_child_behavior_resolver(&self, resolver: Arc<dyn ChildBehaviorResolver>) {
        *self
            .child_behavior
            .write()
            .unwrap_or_else(PoisonError::into_inner) = resolver;
    }

    /// Registers an already-created agent token (notably the session root),
    /// allowing descendants to derive from the exact parent token.
    pub fn register_agent_token(&self, agent_id: AgentId, token: CancellationToken) {
        self.agent_tokens_mut().insert(agent_id, token);
    }

    pub(super) fn agent_tokens_mut(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, HashMap<AgentId, CancellationToken>> {
        self.agent_tokens
            .write()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn agent_tokens_ref(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, HashMap<AgentId, CancellationToken>> {
        self.agent_tokens
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The resolver that decides each child's starting behavior profile.
    pub(super) fn child_behavior_resolver(&self) -> Arc<dyn ChildBehaviorResolver> {
        self.child_behavior
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Performs the budget checks before creating a child task.
    pub async fn max_children_depth_check(
        &self,
        parent_id: AgentId,
        parent_depth: u32,
        parent_budget: &AgentBudget,
    ) -> Result<(), SupervisorError> {
        let children = self.children_of.read().await;
        let current = children.get(&parent_id).map_or(0, |ids| ids.len()) as u32;
        parent_budget.check_children(current)?;
        parent_budget.check_depth(parent_depth)?;
        Ok(())
    }

    /// Purely computes self, descendant, and inclusive usage.
    pub fn inclusive_usage(&self, agent: &Agent) -> rusty_agent_core::usage::AgentUsageSummary {
        let self_metrics = agent.usage.self_metrics();
        let children: Vec<rusty_agent_core::usage::AgentUsageSummary> =
            agent.usage.child_usage.values().cloned().collect();
        rusty_agent_core::usage::compute_agent_usage_summary(self_metrics, &children)
    }
}

#[cfg(test)]
mod tests;
