//! When a checkpoint (a snapshot) is wanted, and who is told.

use rusty_protocol::ids::SessionId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointReason {
    TerminalRun,
    DurableEventCount,
    Explicit,
}

pub trait CheckpointRequester: Send + Sync {
    fn request_checkpoint(&self, session_id: SessionId, at_sequence: u64, reason: CheckpointReason);
}
