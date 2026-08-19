//! Thread-safe in-memory registry for active sessions and capacity permits.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, RwLock};

use harness_protocol::ids::SessionId;

use crate::session_runtime::SessionRuntime;

/// Manages the active in-memory session runtimes and their associated scheduler permits.
///
/// This component isolates concurrency and state synchronization for active sessions,
/// fulfilling the Single Responsibility Principle (SRP).
#[derive(Default)]
pub struct ActiveSessionRegistry {
    sessions: RwLock<HashMap<SessionId, Arc<SessionRuntime>>>,
    session_permits: RwLock<HashMap<SessionId, OwnedSemaphorePermit>>,
}

impl ActiveSessionRegistry {
    /// Creates a new, empty active session registry.
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            session_permits: RwLock::new(HashMap::new()),
        }
    }

    /// Registers a newly created or restored session along with its capacity permit.
    pub async fn register(
        &self,
        id: SessionId,
        runtime: Arc<SessionRuntime>,
        permit: OwnedSemaphorePermit,
    ) {
        self.sessions.write().await.insert(id, runtime);
        self.session_permits.write().await.insert(id, permit);
    }

    /// Retrieves an active session runtime handle by its session ID.
    pub async fn get(&self, id: SessionId) -> Option<Arc<SessionRuntime>> {
        self.sessions.read().await.get(&id).cloned()
    }

    /// Checks if a session is currently registered as active.
    pub async fn contains(&self, id: SessionId) -> bool {
        self.sessions.read().await.contains_key(&id)
    }

    /// Removes an active session and its permit from the registry.
    ///
    /// Returns the session runtime handle if it was present.
    pub async fn remove(
        &self,
        id: SessionId,
    ) -> Option<(Arc<SessionRuntime>, Option<OwnedSemaphorePermit>)> {
        let runtime = self.sessions.write().await.remove(&id)?;
        let permit = self.session_permits.write().await.remove(&id);
        Some((runtime, permit))
    }

    /// Returns the count of currently active sessions.
    pub async fn count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// Returns a list of all active session IDs.
    pub async fn ids(&self) -> Vec<SessionId> {
        self.sessions.read().await.keys().copied().collect()
    }
}
