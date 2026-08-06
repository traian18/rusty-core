//! Explicit lifecycle domains and terminal run outcomes.
//!
//! These types are transport-safe projections. They deliberately keep daemon,
//! connection, session, run, and agent state separate so a terminal run cannot
//! be mistaken for a closed session or daemon.

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, BackendId, IntegrationId, RequestId, RunId, SessionId, Timestamp};

/// Lifecycle of the daemon process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DaemonState {
    Starting,
    Ready,
    ShuttingDown,
    Stopped,
    Failed,
}

/// Lifecycle of one client connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Closing,
    Closed,
}

/// Lifecycle of a session mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionState {
    Open,
    Closing,
    Closed,
    Failed,
}

/// Lifecycle of one admitted run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RunState {
    Queued,
    Running,
    Cancelling,
    Succeeded,
    Cancelled,
    Incomplete,
    Failed,
}

impl RunState {
    /// Whether no further run work may be started for this state.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Cancelled | Self::Incomplete | Self::Failed
        )
    }

    /// Whether cancellation can still affect active work.
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Cancelling)
    }
}

/// Coarse projection of one agent independent from session and run state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentProjection {
    Idle,
    Running,
    WaitingForPermission,
    Completed,
    Cancelled,
    Failed,
}

/// A safe, typed description of a run failure.
///
/// safe_message is suitable for clients and must not contain secrets. IDs
/// allow operators to correlate a failure with a request/provider attempt
/// without exposing provider internals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFailure {
    pub code: String,
    pub safe_message: String,
    pub retryable: bool,
    pub request_id: Option<RequestId>,
    pub backend_id: Option<BackendId>,
    pub integration_id: Option<IntegrationId>,
}

impl RunFailure {
    pub fn new(code: impl Into<String>, safe_message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            safe_message: safe_message.into(),
            retryable: false,
            request_id: None,
            backend_id: None,
            integration_id: None,
        }
    }
}

/// Terminal result of a run, distinct from the session mailbox lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunOutcome {
    Success,
    Cancelled { reason: Option<String> },
    Incomplete { reason: String },
    Failed { failure: RunFailure },
}

/// Explicit identity and terminal projection for one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub state: RunState,
    pub outcome: Option<RunOutcome>,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
}

impl RunRecord {
    pub fn new(run_id: RunId, session_id: SessionId, agent_id: AgentId) -> Self {
        Self {
            run_id,
            session_id,
            agent_id,
            state: RunState::Queued,
            outcome: None,
            started_at: None,
            finished_at: None,
        }
    }

    /// Mark a terminal outcome while keeping the run identity explicit.
    pub fn finish(&mut self, outcome: RunOutcome, finished_at: Timestamp) {
        self.state = match &outcome {
            RunOutcome::Success => RunState::Succeeded,
            RunOutcome::Cancelled { .. } => RunState::Cancelled,
            RunOutcome::Incomplete { .. } => RunState::Incomplete,
            RunOutcome::Failed { .. } => RunState::Failed,
        };
        self.outcome = Some(outcome);
        self.finished_at = Some(finished_at);
    }
}

/// Typed result of cancel_run(run_id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CancelRunOutcome {
    Accepted,
    NotFound,
    AlreadyTerminal { state: RunState },
    NotActive { state: RunState },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states_do_not_include_session_or_daemon_closure() {
        assert!(RunState::Succeeded.is_terminal());
        assert!(RunState::Cancelled.is_terminal());
        assert!(RunState::Incomplete.is_terminal());
        assert!(RunState::Failed.is_terminal());
        assert!(!RunState::Running.is_terminal());
        assert!(!RunState::Queued.is_terminal());
    }

    #[test]
    fn finish_preserves_run_and_session_identity() {
        let run_id = RunId::new();
        let session_id = SessionId::new();
        let agent_id = AgentId::new();
        let mut record = RunRecord::new(run_id, session_id, agent_id);

        record.finish(
            RunOutcome::Failed {
                failure: RunFailure::new("BACKEND_UNAVAILABLE", "backend unavailable"),
            },
            Timestamp::from_sequence(4),
        );

        assert_eq!(record.run_id, run_id);
        assert_eq!(record.session_id, session_id);
        assert_eq!(record.agent_id, agent_id);
        assert_eq!(record.state, RunState::Failed);
        assert!(record.outcome.is_some());
        assert_eq!(record.finished_at, Some(Timestamp::from_sequence(4)));
    }

    #[test]
    fn cancellation_outcomes_are_distinguishable() {
        let outcomes = [
            CancelRunOutcome::Accepted,
            CancelRunOutcome::NotFound,
            CancelRunOutcome::AlreadyTerminal {
                state: RunState::Succeeded,
            },
            CancelRunOutcome::NotActive {
                state: RunState::Queued,
            },
        ];

        for outcome in outcomes {
            let json = serde_json::to_string(&outcome).expect("serialize");
            let decoded: CancelRunOutcome =
                serde_json::from_str(&json).expect("deserialize");
            assert_eq!(decoded, outcome);
        }
    }
}
