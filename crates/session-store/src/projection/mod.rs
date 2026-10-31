//! Applies validated trailing events to a snapshot, without side effects.
//!
//! Only what the durable events carry is applied. Transcript text, tool inputs, backends and
//! child state that the event schema does not contain are never invented.

mod apply;

use crate::{
    replay::{final_sequence, ReplayError},
    store::{DurableSessionEvent, DurableSessionSnapshot, StoredAgentState},
};

/// Brings a migrated checkpoint up to date with the events recorded after it. The result is the
/// exact input to runtime reconstruction.
///
/// An event for an unknown agent, or one that contradicts the checkpoint, is an error: state is
/// never dropped or replaced with a guess.
pub fn replay_snapshot(
    mut snapshot: DurableSessionSnapshot,
    events: &[DurableSessionEvent],
) -> Result<DurableSessionSnapshot, ReplayError> {
    for event in events {
        let sequence = final_sequence(event)?;
        let agent = find_agent(&mut snapshot.agents, event, sequence)?;
        apply::apply_event(agent, &event.envelope, sequence)?;
        snapshot.session_sequence = sequence;
        snapshot.timestamp = event.envelope.timestamp;
    }
    Ok(snapshot)
}

fn find_agent<'a>(
    agents: &'a mut [StoredAgentState],
    event: &DurableSessionEvent,
    sequence: u64,
) -> Result<&'a mut StoredAgentState, ReplayError> {
    let agent_id = event.envelope.agent_id;
    agents
        .iter_mut()
        .find(|agent| agent.agent_id == agent_id)
        .ok_or_else(|| ReplayError::InvalidTransition {
            session_sequence: sequence,
            reason: format!("event targets agent {agent_id}, which is absent from the checkpoint"),
        })
}

#[cfg(test)]
mod tests;
