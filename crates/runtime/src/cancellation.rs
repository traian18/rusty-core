//! Hierarchical cancellation for agent runs (spec §35).
//!
//! Tokio's [`CancellationToken`] forms the backbone of cancellation in the
//! harness.  Each session holds a **root** [`SessionCancellation`]; every
//! agent runner, backend execution, and tool invocation derives a child token
//! from it.  Cancelling the root immediately propagates to every descendant,
//! causing in-flight work to drain promptly.

use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// SessionCancellation
// ---------------------------------------------------------------------------

/// A hierarchical cancellation root scoped to a single session.
///
/// # Usage
///
/// ```ignore
/// let root = SessionCancellation::new();
/// let child = root.child_token();          // for one agent runner
/// let backend_token = child.child_token(); // for a backend call
///
/// // Cancel everything in the session:
/// root.cancel();
/// assert!(child.is_cancelled());
/// assert!(backend_token.is_cancelled());
/// ```
///
/// # Thread safety
///
/// All methods are lock-free and may be called from any thread.  `cancel()`
/// is idempotent — calling it multiple times has no additional effect.
#[derive(Clone)]
pub struct SessionCancellation {
    root: CancellationToken,
}

impl SessionCancellation {
    /// Creates a new, uncancelled root token.
    pub fn new() -> Self {
        Self {
            root: CancellationToken::new(),
        }
    }

    /// Returns a child token linked to the root.
    ///
    /// When the root is cancelled, all child tokens (and their descendants)
    /// are also cancelled.
    pub fn child_token(&self) -> CancellationToken {
        self.root.child_token()
    }

    /// Cancels the root token and all derived child tokens.
    ///
    /// Idempotent — subsequent calls are no-ops.
    pub fn cancel(&self) {
        self.root.cancel();
    }

    /// Returns `true` if cancellation has been requested on this root
    /// (or on any ancestor token in the hierarchy).
    pub fn is_cancelled(&self) -> bool {
        self.root.is_cancelled()
    }
}

impl Default for SessionCancellation {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_token_is_not_cancelled() {
        let sc = SessionCancellation::new();
        assert!(!sc.is_cancelled());
    }

    #[test]
    fn cancel_triggers_is_cancelled() {
        let sc = SessionCancellation::new();
        sc.cancel();
        assert!(sc.is_cancelled());
    }

    #[test]
    fn child_token_cascades_cancellation() {
        let root = SessionCancellation::new();
        let child = root.child_token();
        let grandchild = child.child_token();

        assert!(!child.is_cancelled());
        assert!(!grandchild.is_cancelled());

        root.cancel();

        assert!(child.is_cancelled());
        assert!(grandchild.is_cancelled());
    }

    #[test]
    fn cancel_is_idempotent() {
        let sc = SessionCancellation::new();
        sc.cancel();
        sc.cancel(); // second call should not panic
        assert!(sc.is_cancelled());
    }

    #[test]
    fn default_is_uncancelled() {
        let sc = SessionCancellation::default();
        assert!(!sc.is_cancelled());
    }
}
