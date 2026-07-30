//! Resource access control for the harness runtime.
//!
//! [`ResourceManager`] provides a lightweight, async‑safe mechanism for
//! coordinating access to named resources (files, git repositories, terminals,
//! etc.) across multiple agent sessions. Resources can be acquired in either
//! shared or exclusive mode:
//!
//! - **Shared** — many sessions may hold the resource concurrently, as long as
//!   no session holds it exclusively.
//! - **Exclusive** — only one session may hold the resource, and no shared
//!   holders are permitted.
//!
//! This is a scaffolding module intended to support future `fs.edit`‑style
//! locking. The current implementation is correct but minimal; it does not
//! yet implement deadlock detection, timeouts, or upgrade/downgrade of locks.

use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::Mutex;

use harness_protocol::ids::SessionId;

/// Identifies a resource that can be acquired or released.
///
/// Each variant describes a category of resource, with an inner value that
/// uniquely identifies the specific resource within that category.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResourceKey {
    /// A file on disk, identified by its absolute or canonical path.
    File(PathBuf),
    /// A Git repository working tree, identified by its root path.
    GitRepository(PathBuf),
    /// A logical workspace (e.g., a project or scratchpad).
    Workspace(String),
    /// A terminal emulator instance.
    Terminal(String),
    /// A custom resource identified by an arbitrary string.
    Custom(String),
}

/// The access mode requested for a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    /// Multiple sessions may hold the resource simultaneously, provided no
    /// session holds it exclusively.
    Shared,
    /// Only one session may hold the resource; any existing shared or
    /// exclusive holders cause acquisition to fail.
    Exclusive,
}

/// Errors that can occur during resource acquisition or release.
#[derive(Debug, thiserror::Error)]
pub enum ResourceError {
    /// The resource is already held in exclusive mode (or, when requesting
    /// exclusive access, held in any mode).
    #[error("resource {0:?} is exclusively held by another session")]
    ExclusivelyHeld(ResourceKey),
}

/// Tracks the set of sessions that currently hold a particular resource.
struct Holders {
    /// Sessions that hold the resource in shared (read) mode.
    shared: Vec<SessionId>,
    /// Optional session that holds the resource in exclusive (write) mode.
    exclusive: Option<SessionId>,
}

/// Async‑safe manager for resource acquisition and release.
///
/// Internally, state is stored in a `tokio::sync::Mutex<HashMap<ResourceKey,
/// Holders>>`, so all public operations are `async` and safe to call from
/// multiple tasks concurrently.
///
/// # Example (sketch)
///
/// ```ignore
/// let mgr = ResourceManager::new();
/// let key = ResourceKey::File("/tmp/data.json".into());
/// let sid = SessionId::new();
///
/// // Acquire shared access
/// mgr.acquire(key.clone(), AccessMode::Shared, sid).await.unwrap();
///
/// // Release
/// mgr.release(key, sid).await;
/// ```
pub struct ResourceManager {
    state: Mutex<HashMap<ResourceKey, Holders>>,
}

impl ResourceManager {
    /// Creates a new, empty `ResourceManager`.
    ///
    /// No resources are tracked until the first call to [`acquire`](Self::acquire).
    pub fn new() -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Attempts to acquire `key` in the given `mode` on behalf of `holder`.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceError::ExclusivelyHeld`] when:
    /// - The resource is already held in exclusive mode (regardless of the
    ///   requested mode).
    /// - Exclusive access is requested but the resource is already held by
    ///   one or more shared holders.
    pub async fn acquire(
        &self,
        key: ResourceKey,
        mode: AccessMode,
        holder: SessionId,
    ) -> Result<(), ResourceError> {
        let mut state = self.state.lock().await;
        let entry = state
            .entry(key.clone())
            .or_insert(Holders {
                shared: vec![],
                exclusive: None,
            });

        match mode {
            AccessMode::Shared => {
                if entry.exclusive.is_some() {
                    return Err(ResourceError::ExclusivelyHeld(key));
                }
                entry.shared.push(holder);
                Ok(())
            }
            AccessMode::Exclusive => {
                if entry.exclusive.is_some() || !entry.shared.is_empty() {
                    return Err(ResourceError::ExclusivelyHeld(key));
                }
                entry.exclusive = Some(holder);
                Ok(())
            }
        }
    }

    /// Releases the hold on `key` by `holder`.
    ///
    /// If the holder was a shared participant it is removed from the shared
    /// list. If it was the exclusive holder the exclusive slot is cleared.
    ///
    /// This is a "best effort" operation — it silently succeeds even if
    /// `holder` did not actually hold the resource, which simplifies cleanup
    /// during error recovery.
    pub async fn release(&self, key: ResourceKey, holder: SessionId) {
        let mut state = self.state.lock().await;
        if let Some(entry) = state.get_mut(&key) {
            entry.shared.retain(|id| *id != holder);
            if entry.exclusive == Some(holder) {
                entry.exclusive = None;
            }
        }
    }
}

impl Default for ResourceManager {
    fn default() -> Self {
        Self::new()
    }
}
