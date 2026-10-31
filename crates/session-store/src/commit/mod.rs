//! The commit boundary: the one place an event gets its final session sequence and is handed to
//! the store, so that what clients see and what is persisted cannot disagree.
//!
//! - [`SessionSequencer`] hands out gap-free sequences, resuming from the store after a restart.
//! - [`SessionCommitter`] sequences, persists (according to its [`DurabilityPolicy`]) and asks for
//!   checkpoints at terminal runs and every `snapshot_every` durable events.
//! - [`RecordingSink`] collects what was published, for tests and diagnostics.

mod checkpoint;
mod committer;
mod recording_sink;
mod sequencer;

pub use checkpoint::{CheckpointReason, CheckpointRequester};
pub use committer::{SessionCommitter, DEFAULT_SNAPSHOT_EVERY};
pub use recording_sink::RecordingSink;
pub use sequencer::SessionSequencer;

use rusty_protocol::ids::SessionId;

use crate::store::StoreError;

/// Why an event could not be committed.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CommitError {
    #[error("durable store error: {0}")]
    Store(#[from] StoreError),
    #[error("duplicate session sequence {0}")]
    DuplicateSequence(u64),
    #[error("session store for {0} is closed")]
    Closed(SessionId),
    #[error("invalid commit payload: {0}")]
    InvalidEvent(String),
}

/// What a commit does when the store fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DurabilityPolicy {
    #[default]
    /// The commit fails with the store's error.
    Strict,
    /// The event is still published, marked `degraded`, so a store outage does not stop the run.
    BestEffort,
}

/// An event after commit: sequenced, and flagged when it could not be stored.
#[derive(Debug, Clone)]
pub struct CommittedEvent {
    pub envelope: rusty_protocol::events::AgentEventEnvelope,
    pub degraded: bool,
}

#[cfg(test)]
mod tests;
