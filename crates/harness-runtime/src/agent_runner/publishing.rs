//! Emitting agent events: sequencing, the session's commit boundary, and
//! publication to subscribers.

use harness_protocol::effects::SessionMutation;
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, AgentOutcome, EventVisibility};
use harness_protocol::ids::{EventId, Timestamp};
use harness_session_store::SessionCommitter;

use super::AgentRunner;

impl AgentRunner {
    /// Emits `event`; `false` when it could not be committed, which fails
    /// the session (see [`Self::commit_and_publish`]).
    pub(super) async fn emit(&mut self, event: AgentEvent) -> bool {
        self.record_outcome(&event);
        let envelope = self.envelope(event);

        // RC-301: when the session's authoritative committer is configured,
        // every event flows through the one commit boundary — the sequencer
        // assigns the final sequence, durable events are persisted with it,
        // and the committed envelope is published only when persistence
        // succeeded (strict policy) or explicitly degraded (best-effort).
        if let Some(committer) = self.committer.clone() {
            return self.commit_and_publish(&committer, envelope).await;
        }

        // Legacy path (no store configured): persist durable events directly
        // and let the session bus assign sequences.
        if harness_session_store::is_durable(&envelope.event) {
            self.persist(SessionMutation::AppendEvent(envelope.clone()))
                .await;
        }
        self.publish(envelope);
        true
    }

    fn record_outcome(&self, event: &AgentEvent) {
        let outcome = match event {
            AgentEvent::Completed { outcome } => *outcome,
            AgentEvent::Failed { .. } => AgentOutcome::Failed,
            _ => return,
        };
        let mut table = self.live_state.lock().expect("live_state mutex poisoned");
        let entry = table.entry(self.agent.id).or_default();
        entry.last_outcome = Some(outcome);
    }

    fn envelope(&mut self, event: AgentEvent) -> AgentEventEnvelope {
        let envelope = AgentEventEnvelope {
            event_id: EventId::new(),
            session_id: self.agent.session_id,
            agent_id: self.agent.id,
            parent_agent_id: self.agent.parent_id,
            run_id: self.agent.state.active_run,
            agent_sequence: self.agent_sequence,
            session_sequence: None,
            timestamp: Timestamp::now(),
            visibility: EventVisibility::User,
            event,
        };
        self.agent_sequence = self.agent_sequence.wrapping_add(1);
        envelope
    }

    async fn commit_and_publish(
        &mut self,
        committer: &SessionCommitter,
        envelope: AgentEventEnvelope,
    ) -> bool {
        match committer.commit(envelope).await {
            Ok(Some(committed)) => {
                checkpoint_if_terminal(committer, &committed.envelope);
                self.publish(committed.envelope);
                true
            }
            Ok(None) => {
                // Strict policy: persistence failed, so the event is
                // never published. The session state has already
                // advanced; the runtime must treat this as a
                // session-level failure.
                tracing::error!(
                    session_id = %self.agent.session_id,
                    agent_id = %self.agent.id,
                    "commit boundary rejected an event; it was not published"
                );
                self.fail_fatally(
                    "PERSISTENCE_COMMIT_REJECTED",
                    "strict event commit rejected publication".into(),
                );
                false
            }
            Err(error) => {
                tracing::error!(
                    session_id = %self.agent.session_id,
                    agent_id = %self.agent.id,
                    %error,
                    "commit boundary failed; event was not published"
                );
                self.fail_fatally(
                    "PERSISTENCE_COMMIT_FAILED",
                    format!("strict event commit failed: {error}"),
                );
                false
            }
        }
    }

    fn publish(&self, envelope: AgentEventEnvelope) {
        // The agent's own channel only has receivers while someone watches
        // this agent directly; the session sink below always gets the event.
        if self.task.events.send(envelope.clone()).is_err() {
            tracing::trace!("no direct subscriber for agent event");
        }
        self.event_sink.send(envelope);
    }
}

/// RC-302: a terminal run triggers an automatic checkpoint at the committed
/// sequence.
fn checkpoint_if_terminal(committer: &SessionCommitter, envelope: &AgentEventEnvelope) {
    let terminal = matches!(
        &envelope.event,
        AgentEvent::Completed { .. } | AgentEvent::Failed { .. }
    );
    if let (true, Some(sequence)) = (terminal, envelope.session_sequence) {
        committer.checkpoint_for_terminal_run(sequence);
    }
}
