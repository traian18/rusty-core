//! What a client can tell a running session: prompts, steering, cancellation, permission answers.

use super::{SessionCommand, SessionError, SessionRuntime, SessionStatus};
use crate::locks::MutexExt;

use rusty_protocol::commands::{AgentCommand, PermissionDecision, UserInput};

use rusty_protocol::ids::PermissionId;

impl SessionRuntime {
    /// Cancel only the root agent's active run.
    ///
    /// This is intentionally mailbox-scoped rather than using the session
    /// cancellation root: `AgentCommand::Cancel` cancels the active backend,
    /// tools, and children through their run-scoped tokens, while the
    /// long-lived root runner remains available to process queued follow-ups
    /// and future prompts.
    pub async fn cancel_run(&self) -> Result<(), SessionError> {
        self.root_agent_tx
            .send(AgentCommand::Cancel)
            .await
            .map_err(|_| SessionError::ChannelClosed)
    }

    /// Send steering input to the root agent.
    ///
    /// Delivery is serialized through the root agent mailbox, preserving
    /// transcript ordering with backend and tool events.
    pub async fn steer(&self, input: UserInput) -> Result<(), SessionError> {
        self.root_agent_tx
            .send(AgentCommand::Steer { input })
            .await
            .map_err(|_| SessionError::ChannelClosed)
    }

    /// Queue a follow-up prompt for the root agent.
    pub async fn follow_up(&self, input: UserInput) -> Result<(), SessionError> {
        self.root_agent_tx
            .send(AgentCommand::FollowUp { input })
            .await
            .map_err(|_| SessionError::ChannelClosed)
    }

    /// Resolve a pending root-agent permission request.
    pub async fn resolve_permission(
        &self,
        id: PermissionId,
        decision: PermissionDecision,
    ) -> Result<(), SessionError> {
        self.root_agent_tx
            .send(AgentCommand::PermissionResolved { id, decision })
            .await
            .map_err(|_| SessionError::ChannelClosed)
    }

    /// Send a command to the root agent's task channel.
    ///
    /// In this Phase 2 implementation, only the root agent is supported.
    ///
    /// When a [`SessionCommand::Prompt`] is sent, the session-level status
    /// transitions to [`SessionStatus::Running`] optimistically (the agent
    /// runner processes the `StartRun` asynchronously).  This ensures that
    /// [`state_snapshot`](SessionRuntime::state_snapshot) returns an accurate
    /// active status from the moment the command is issued.
    pub async fn send_command(&self, command: SessionCommand) -> Result<(), SessionError> {
        let agent_command = match command {
            SessionCommand::Prompt(input) => {
                if self.cancellation.is_cancelled() {
                    return Err(SessionError::Cancelled);
                }
                // Optimistically transition to Running so that state_snapshot
                // reflects the session as active immediately.
                let mut state = self.state.lock_or_recover();
                if matches!(
                    state.status,
                    SessionStatus::Idle | SessionStatus::Completed | SessionStatus::Cancelled
                ) {
                    state.status = SessionStatus::Running;
                    state.error = None;
                }
                drop(state);
                AgentCommand::StartRun { input }
            }
            SessionCommand::SpawnChild(spec) => AgentCommand::SpawnChild { spec },
            SessionCommand::Cancel => AgentCommand::Cancel,
            SessionCommand::Pause => AgentCommand::Pause,
            SessionCommand::Resume => AgentCommand::Resume,
            SessionCommand::ConfigureExecution(params) => {
                AgentCommand::ConfigureExecution { params }
            }
            SessionCommand::SetBehaviorProfile(profile) => AgentCommand::SetBehaviorProfile {
                profile,
                library: Vec::new(),
                allow_commands: false,
            },
            SessionCommand::SetBehaviorBundle {
                profile,
                library,
                allow_commands,
            } => AgentCommand::SetBehaviorProfile {
                profile,
                library,
                allow_commands,
            },
        };

        self.root_agent_tx
            .send(agent_command)
            .await
            .map_err(|_| SessionError::ChannelClosed)
    }
}
