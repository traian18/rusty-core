//! Child agents: spawned, completed or failed.

use rusty_protocol::commands::{AgentError, AgentResult, AgentStatus};
use rusty_protocol::effects::AgentEffect;
use rusty_protocol::events::{AgentEvent, AgentOutcome};
use rusty_protocol::ids::AgentId;
use rusty_protocol::usage::AgentUsageSummary;

use crate::agent::Agent;

impl Agent {
    pub(super) fn child_completed(
        &mut self,
        agent_id: AgentId,
        result: AgentResult,
    ) -> Vec<AgentEffect> {
        self.state.children.retain(|child| *child != agent_id);
        self.usage
            .add_child_summary(agent_id, Self::bridge_usage_summary(&result.usage));
        let mut effects = vec![AgentEffect::Emit {
            event: AgentEvent::ChildAgentCompleted {
                agent_id,
                outcome: AgentOutcome::Success,
            },
        }];
        if self.state.children.is_empty() && self.state.status == AgentStatus::WaitingForChildren {
            self.state.status = AgentStatus::Idle;
            effects.push(AgentEffect::Emit {
                event: AgentEvent::StateChanged {
                    from: AgentStatus::WaitingForChildren,
                    to: AgentStatus::Idle,
                },
            });
        }
        effects
    }

    pub(super) fn child_spawned(&mut self, agent_id: AgentId, awaiting: bool) -> Vec<AgentEffect> {
        if !self.state.children.contains(&agent_id) {
            self.state.children.push(agent_id);
        }
        if !awaiting || self.state.status == AgentStatus::WaitingForChildren {
            return Vec::new();
        }
        let from = self.state.status;
        self.state.status = AgentStatus::WaitingForChildren;
        vec![Self::state_changed(from, AgentStatus::WaitingForChildren)]
    }

    pub(super) fn child_failed(
        &mut self,
        agent_id: AgentId,
        error: AgentError,
    ) -> Vec<AgentEffect> {
        self.state.children.retain(|child| *child != agent_id);
        self.state.last_error = Some(error);
        let mut effects = vec![AgentEffect::Emit {
            event: AgentEvent::ChildAgentCompleted {
                agent_id,
                outcome: AgentOutcome::Failed,
            },
        }];
        if self.state.children.is_empty() && self.state.status == AgentStatus::WaitingForChildren {
            self.state.status = AgentStatus::Idle;
            effects.push(AgentEffect::Emit {
                event: AgentEvent::StateChanged {
                    from: AgentStatus::WaitingForChildren,
                    to: AgentStatus::Idle,
                },
            });
        }
        effects
    }

    /// Lossless conversion from a completed child's reported (protocol-level)
    /// usage summary into the shape this agent's ledger stores its children
    /// under. Both types have identical fields — see
    /// `crate::usage::AgentUsageSummary`'s doc comment for why the type
    /// still exists separately from the protocol one.
    pub(super) fn bridge_usage_summary(
        protocol: &rusty_protocol::usage::AgentUsageSummary,
    ) -> crate::usage::AgentUsageSummary {
        crate::usage::AgentUsageSummary::from(protocol.clone())
    }

    /// Builds this agent's final, reportable usage summary: its own request
    /// count / tool-call count / tokens / cost (`self.usage.self_metrics()`),
    /// combined with every child's already-aggregated inclusive usage
    /// (`self.usage.child_usage`) via `compute_agent_usage_summary`, which
    /// sums children's *inclusive* usage — not `self_usage` — so a
    /// grandchild's activity is counted exactly once even though it reaches
    /// this agent through two levels of `ChildCompleted` bridging.
    pub(super) fn terminal_usage_summary(&self) -> AgentUsageSummary {
        let self_metrics = self.usage.self_metrics();
        let child_summaries: Vec<crate::usage::AgentUsageSummary> =
            self.usage.child_usage.values().cloned().collect();
        crate::usage::compute_agent_usage_summary(self_metrics, &child_summaries).into()
    }
}
