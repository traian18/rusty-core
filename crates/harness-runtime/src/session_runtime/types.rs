//! Small session-level value types: status, commands, errors, and the
//! read-only snapshot/state projections.

use std::collections::HashMap;

use harness_core::agent::Agent;
use harness_protocol::backend::ExecutionParams;
use harness_protocol::commands::UserInput;
use harness_protocol::effects::SpawnAgentSpec;
use harness_protocol::ids::{AgentId, SessionId};

// ---------------------------------------------------------------------------
// SessionStatus
// ---------------------------------------------------------------------------

/// The high-level status of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionStatus {
    /// Session is ready to accept commands.
    Idle,
    /// A run is in progress on at least one agent.
    Running,
    /// All agent runs have completed successfully.
    Completed,
    /// The session was cancelled before all runs completed.
    Cancelled,
    /// The root task panicked or encountered an unrecoverable error.
    Failed,
}

// ---------------------------------------------------------------------------
// SessionCommand
// ---------------------------------------------------------------------------

/// A command sent to a session to drive its execution.
///
/// These are the user-facing commands that the session translates into
/// [`AgentCommand`](harness_protocol::commands::AgentCommand)s for the
/// appropriate agent runner.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum SessionCommand {
    /// Start a new run with the given user input.
    Prompt(UserInput),
    /// Spawn a child from the root agent through the normal effect path.
    SpawnChild(SpawnAgentSpec),
    /// Cancel the entire session.
    Cancel,
    /// Pause the session.
    Pause,
    /// Resume a paused session.
    Resume,
    /// Update the root agent's session-level default execution params
    /// (model, max_tokens, temperature, reasoning, ...). See
    /// `AgentCommand::ConfigureExecution`.
    ConfigureExecution(ExecutionParams),
}

// ---------------------------------------------------------------------------
// SessionError
// ---------------------------------------------------------------------------

/// Errors that can occur during session operations.
#[derive(Debug, Clone)]
pub enum SessionError {
    /// The session was not found.
    SessionNotFound,
    /// The session is in an invalid state for the requested operation.
    InvalidState {
        /// The expected state.
        expected: String,
        /// The actual state.
        actual: String,
    },
    /// The operation was cancelled.
    Cancelled,
    /// A command channel was closed unexpectedly.
    ChannelClosed,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::SessionNotFound => write!(f, "session not found"),
            SessionError::InvalidState { expected, actual } => {
                write!(f, "invalid state: expected {expected}, actual {actual}")
            }
            SessionError::Cancelled => write!(f, "operation cancelled"),
            SessionError::ChannelClosed => write!(f, "command channel closed"),
        }
    }
}

impl std::error::Error for SessionError {}

// ---------------------------------------------------------------------------
// SessionSnapshot
// ---------------------------------------------------------------------------

/// A lightweight read projection of the session's current state.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    /// The session's unique identifier.
    pub session_id: SessionId,
    /// The session's current status.
    pub status: SessionStatus,
    /// The root agent's identifier.
    pub root_agent_id: AgentId,
    /// The number of agents in the session.
    pub agent_count: usize,
    /// An optional error message, populated when the session is [`SessionStatus::Failed`].
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// SessionState
// ---------------------------------------------------------------------------

/// The durable state of a session.
#[derive(Debug, Clone)]
pub struct SessionState {
    /// All agents in this session, indexed by their [`AgentId`].
    pub agents: HashMap<AgentId, Agent>,
    /// The identifier of the root agent (created when the session starts).
    pub root_agent_id: AgentId,
    /// The session's current status.
    pub status: SessionStatus,
    /// An optional error message, populated when the session enters [`SessionStatus::Failed`].
    pub error: Option<String>,
}
