//! Root agent task supervision and failure propagation.

use std::sync::Arc;

use harness_protocol::ids::SessionId;

use crate::session_runtime::SessionRuntime;

/// Monitors the root agent task execution of an active session.
///
/// Encapsulates the supervisor loop that listens for unexpected task exits or panics
/// and transitions the session runtime into a truthful failed state.
pub struct SessionSupervisor;

impl SessionSupervisor {
    /// Starts background supervision for a session's root agent task if a join handle exists.
    ///
    /// If the root task fails with a join error (e.g., panicked or aborted), the supervisor
    /// marks the runtime as failed.
    pub fn supervise(session_id: SessionId, runtime: &Arc<SessionRuntime>) {
        if let Some(handle) = runtime.take_root_task_handle() {
            let runtime_for_supervisor = runtime.clone();
            tokio::spawn(async move {
                if let Err(join_err) = handle.await {
                    tracing::error!(%session_id, error = %join_err, "session root task failed");
                    runtime_for_supervisor.mark_failed(join_err.to_string());
                }
            });
        }
    }
}
