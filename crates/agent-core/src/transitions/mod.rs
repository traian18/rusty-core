//! The reducer: how an agent turns one command into its effects, without side effects.
//!
//! [`Agent::apply`] is the table of "command -> what happens". Each arm calls one operation, and
//! each operation lives in its own module (`run_start`, `tool_request`, `completion`, ...).
//! Backend-event handling is in `backend` and `backend_events`.

mod assistant_text;
mod backend_events;
mod behavior_profile;
mod children;
mod completion;
mod control;
mod ids;
mod lifecycle;
mod run_start;
mod tool_request;
mod tool_result;
mod turn;

pub use tool_result::MAX_CONSECUTIVE_TOOL_TIMEOUTS;

use rusty_protocol::{commands::AgentCommand, effects::AgentEffect};

use crate::agent::Agent;

impl Agent {
    /// Turns one command into the effects it causes, updating the agent's state. It never does
    /// I/O: the runtime carries the effects out and reports the outcomes as the next commands.
    pub fn apply(&mut self, command: AgentCommand) -> Vec<AgentEffect> {
        match command {
            AgentCommand::StartRun { input }
            | AgentCommand::Steer { input }
            | AgentCommand::FollowUp { input } => self.start_or_queue(input),
            AgentCommand::StartNextQueuedRun => self.start_next_queued_run(),
            AgentCommand::BackendEvent { run_id, event } => self.backend_event(run_id, event),
            AgentCommand::ToolCompleted { call_id, result } => self.tool_completed(call_id, result),
            AgentCommand::ToolFailed { call_id, error } => self.tool_failed(call_id, error),
            AgentCommand::PermissionResolved { id, decision } => {
                self.permission_resolved(id, decision)
            }
            AgentCommand::SpawnChild { spec } => vec![AgentEffect::SpawnAgent { spec }],
            AgentCommand::ChildSpawned { agent_id, awaiting } => {
                self.child_spawned(agent_id, awaiting)
            }
            AgentCommand::ChildCompleted { agent_id, result } => {
                self.child_completed(agent_id, result)
            }
            AgentCommand::ChildFailed { agent_id, error } => self.child_failed(agent_id, error),
            AgentCommand::Cancel => self.cancel(),
            AgentCommand::Pause => self.pause(),
            AgentCommand::Resume => self.resume(),
            AgentCommand::ConfigureExecution { params } => self.configure_execution(params),
            AgentCommand::SetBehaviorProfile {
                profile,
                library,
                allow_commands,
            } => self.set_behavior_profile(profile, library, allow_commands),
            AgentCommand::CompletionEvaluated {
                run_id,
                attempt,
                verdicts,
                usage,
            } => self.completion_evaluated(run_id, attempt, verdicts, usage),
        }
    }
}
