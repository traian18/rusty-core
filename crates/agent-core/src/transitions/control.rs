//! Cancel, pause and resume.

use rusty_protocol::commands::AgentStatus;
use rusty_protocol::effects::AgentEffect;
use rusty_protocol::events::{AgentEvent, AgentOutcome};

use crate::agent::Agent;

impl Agent {
    pub(super) fn cancel(&mut self) -> Vec<AgentEffect> {
        let from = self.state.status;
        if matches!(
            from,
            AgentStatus::Idle | AgentStatus::Cancelled | AgentStatus::Failed
        ) && self.state.active_run.is_none()
        {
            return Vec::new();
        }

        let mut effects = Vec::new();
        if let Some(run_id) = self.state.active_run {
            effects.push(AgentEffect::CancelBackend { run_id });
            if from == AgentStatus::Verifying {
                effects.push(AgentEffect::CancelEvaluation { run_id });
            }
        }

        let mut calls: Vec<_> = self.state.pending_tools.keys().copied().collect();
        calls.sort();
        effects.extend(
            calls
                .into_iter()
                .map(|call_id| AgentEffect::CancelTool { call_id }),
        );

        let mut children = self.state.children.clone();
        children.sort();
        effects.extend(
            children
                .into_iter()
                .map(|agent_id| AgentEffect::CancelChild { agent_id }),
        );

        self.state.status = AgentStatus::Cancelled;
        self.state.active_run = None;
        self.state.backend_in_flight = false;
        self.state.pending_tools.clear();
        self.state.pending_permissions.clear();
        self.state.children.clear();
        // `Cancel` is documented and certified (RC-203,
        // `multiple_follow_ups_are_fifo_and_survive_cancellation`) as
        // cancelling only the *current run*: already-queued follow-up/steer
        // input is a separate, already-committed piece of user intent and
        // must remain available for an explicit `StartNextQueuedRun` after
        // the cancel — including across a restore, so a queued follow-up
        // typed just before a crash is not silently lost. Do not clear
        // `queued_inputs` here; a caller that truly wants to abandon queued
        // work (e.g. a full session teardown) should do so explicitly at
        // its own layer instead of narrowing this shared transition.

        effects.push(Self::state_changed(from, AgentStatus::Cancelled));
        effects.push(AgentEffect::Emit {
            event: AgentEvent::Completed {
                outcome: AgentOutcome::Cancelled,
            },
        });
        effects
    }

    pub(super) fn pause(&mut self) -> Vec<AgentEffect> {
        // A completion evaluation is short and bounded; pausing mid-way would
        // discard its verdict and make resume re-ask the model. It is not
        // interruptible by pause (cancel still stops it).
        if matches!(
            self.state.status,
            AgentStatus::Paused
                | AgentStatus::Cancelled
                | AgentStatus::Failed
                | AgentStatus::Verifying
        ) {
            return Vec::new();
        }
        let from = self.state.status;
        self.state.status = AgentStatus::Paused;
        vec![Self::state_changed(from, AgentStatus::Paused)]
    }

    pub(super) fn resume(&mut self) -> Vec<AgentEffect> {
        if self.state.status != AgentStatus::Paused {
            return Vec::new();
        }
        match self.state.active_run {
            Some(run_id) => {
                self.state.status = AgentStatus::WaitingForBackend;
                let request = self.build_request(run_id, true);
                vec![
                    Self::state_changed(AgentStatus::Paused, AgentStatus::WaitingForBackend),
                    AgentEffect::ExecuteBackend { request },
                ]
            }
            None => {
                self.state.status = AgentStatus::Idle;
                vec![Self::state_changed(AgentStatus::Paused, AgentStatus::Idle)]
            }
        }
    }
}
