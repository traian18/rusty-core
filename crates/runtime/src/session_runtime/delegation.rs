//! Sessions this one delegates work to: orchestration steps run in isolated sessions that share its backend and tools.

use super::SessionRuntime;
use crate::locks::MutexExt;
use std::sync::Arc;

use rusty_protocol::ids::SessionId;

impl SessionRuntime {
    /// Make `session` reachable through [`delegated_session`](Self::delegated_session)
    /// while it runs work on this session's behalf.
    pub fn register_delegated(&self, session: &Arc<SessionRuntime>) {
        let mut delegated = self.delegated.lock_or_recover();
        delegated.retain(|_, weak| weak.strong_count() > 0);
        delegated.insert(session.session_id, Arc::downgrade(session));
    }

    pub fn unregister_delegated(&self, session_id: SessionId) {
        self.delegated.lock_or_recover().remove(&session_id);
    }

    /// A live delegated session (see [`register_delegated`](Self::register_delegated)).
    pub fn delegated_session(&self, session_id: SessionId) -> Option<Arc<SessionRuntime>> {
        self.delegated
            .lock_or_recover()
            .get(&session_id)
            .and_then(std::sync::Weak::upgrade)
    }
}
