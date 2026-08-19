//! RC-302 automatic snapshot hook: bridges the session committer's
//! checkpoint requests to an actual durable snapshot save.

use std::sync::Arc;

use harness_protocol::ids::{AgentId, SessionId};
use harness_session_store::{CheckpointReason, CheckpointRequester, SessionStore};

use crate::traits::Workspace;

use super::projection::{build_snapshot, AgentProjectionTable};

/// Bridges the committer's checkpoint hooks to an actual snapshot save.
///
/// Captures the fields a snapshot needs (live projections, store, workspace)
/// so the committer can fire terminal-run and count-based checkpoint
/// requests without holding a reference to the not-yet-constructed
/// [`SessionRuntime`](super::SessionRuntime). The snapshot is built
/// synchronously (all in-memory); only the durable write is spawned.
pub(super) struct RuntimeCheckpointRequester {
    pub(super) session_id: SessionId,
    pub(super) root_agent_id: AgentId,
    pub(super) projection: AgentProjectionTable,
    pub(super) store: Arc<dyn SessionStore>,
    pub(super) workspace: Arc<dyn Workspace>,
}

impl CheckpointRequester for RuntimeCheckpointRequester {
    fn request_checkpoint(
        &self,
        _session_id: SessionId,
        at_sequence: u64,
        reason: CheckpointReason,
    ) {
        let snapshot = build_snapshot(
            self.session_id,
            self.root_agent_id,
            &self.projection,
            self.workspace.as_ref(),
            at_sequence,
            false,
            0,
        );
        let store = self.store.clone();
        tokio::spawn(async move {
            let start = std::time::Instant::now();
            let result = store.save_snapshot(snapshot).await;
            metrics::histogram!("harness_checkpoint_duration_seconds")
                .record(start.elapsed().as_secs_f64());
            match result {
                Ok(()) => metrics::counter!("harness_checkpoints_total", "outcome" => "success")
                    .increment(1),
                Err(error) => {
                    metrics::counter!("harness_checkpoints_total", "outcome" => "failed")
                        .increment(1);
                    tracing::error!(%error, ?reason, "automatic session checkpoint failed");
                }
            }
        });
    }
}
