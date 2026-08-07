//! Typed mutation admission results shared by the daemon and SDKs.
//!
//! Admission is deliberately separate from execution: accepting a command
//! means the daemon has made a durable decision about it, not that its run has
//! completed. Keeping these outcomes in the protocol crate prevents each
//! transport from inventing a different retry or conflict representation.

use serde::{Deserialize, Serialize};

use crate::ids::{RunId, SessionId};

/// Client-generated identity for one mutation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CommandId(pub uuid::Uuid);

impl CommandId {
    /// Create a fresh command identity for a new mutation.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for CommandId {
    fn default() -> Self {
        Self::new()
    }
}

/// Stable result of admitting a mutation into a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum AdmissionResult {
    /// The command was accepted when the runtime cannot expose a concrete run identity.
    Accepted,
    /// The command started a new run immediately.
    AcceptedStarted { run_id: RunId },
    /// The command was retained for FIFO execution.
    AcceptedQueued { run_id: RunId, position: u32 },
    /// The command was applied without starting a run (for example, cancel).
    AcceptedApplied,
    /// The same command identity was seen before; returns its original result.
    Duplicate { original: Box<AdmissionResult> },
    /// The caller used a stale optimistic-concurrency revision.
    RejectedConflict { current_session_revision: u64 },
    /// The target session has been closed.
    RejectedClosed,
    /// The command is not valid for the current lifecycle state.
    RejectedInvalidState { reason: String },
    /// The command exceeded a configured queue or resource limit.
    RejectedCapacity { limit: String },
}

/// Canonical identity and concurrency metadata attached to a mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationMetadata {
    pub command_id: CommandId,
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub expected_session_revision: Option<u64>,
    pub trace_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_identity_and_admission_round_trip() {
        let command_id = CommandId::new();
        let result = AdmissionResult::AcceptedQueued {
            run_id: RunId::new(),
            position: 2,
        };
        let metadata = MutationMetadata {
            command_id,
            session_id: SessionId::new(),
            run_id: None,
            expected_session_revision: Some(7),
            trace_id: Some("trace-1".into()),
        };

        let json = serde_json::to_string(&(metadata, result.clone())).expect("serialize");
        let (_, parsed_result): (MutationMetadata, AdmissionResult) =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed_result, result);
    }

    #[test]
    fn duplicate_preserves_original_outcome() {
        let original = AdmissionResult::AcceptedStarted {
            run_id: RunId::new(),
        };
        let duplicate = AdmissionResult::Duplicate {
            original: Box::new(original.clone()),
        };
        assert_eq!(
            duplicate,
            AdmissionResult::Duplicate {
                original: Box::new(original)
            }
        );
    }
}
