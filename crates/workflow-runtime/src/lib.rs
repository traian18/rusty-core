#![warn(clippy::all)]

//! Runs workflows: executes an orchestration definition step by step, each agent step in its own
//! session, and the persisted serial task queue some steps use. The decisions about what runs
//! next come from `rusty-workflow-core`; sessions come from `rusty-runtime`.

mod markdown_reply;
pub mod orchestration;
pub mod session_agent_executor;
mod task_queue;

/// Checkpoints of the workspace's git repositories under hidden refs, and undoing back to one:
/// the task queue's, also used by hosts that run their own passes (goal loops).
pub mod workspace_snapshot {
    pub use crate::task_queue::snapshot::{
        fingerprint, patch, take, undo_to, RepoSnapshot, Snapshot, Undone,
    };
}
