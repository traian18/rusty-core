//! Per-agent context lifecycle state.
//!
//! Canonical messages remain in the agent state. This state only tracks the
//! bounded inference view and its checkpoint lineage.

use harness_protocol::ids::{ContextCheckpointId, ContextItemId, MessageId, Timestamp};

/// Durable bookkeeping for one agent's prepared inference context.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentContextState {
    /// Latest checkpoint selected for future inference requests.
    pub active_checkpoint: Option<ContextCheckpointId>,
    /// Last canonical message covered by the active checkpoint.
    pub covered_through: Option<MessageId>,
    /// Explicit context items that must survive ordinary packing and compaction.
    pub pinned_items: Vec<ContextItemId>,
    /// Incremented whenever canonical inputs affecting context change.
    pub generation: u64,
    /// Latest projected input size, when a tokenizer or estimator was available.
    pub last_estimated_tokens: Option<u64>,
    /// Stable-boundary timestamp of the latest accepted compaction.
    pub last_compacted_at: Option<Timestamp>,
}

impl AgentContextState {
    /// Marks canonical context as changed and invalidates cached token pressure.
    pub fn advance_generation(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.last_estimated_tokens = None;
    }

    /// Records a completed checkpoint only if it was prepared from the current generation.
    ///
    /// Returning false lets the runtime discard stale asynchronous compaction
    /// results without overwriting a newer checkpoint.
    pub fn accept_checkpoint(
        &mut self,
        prepared_generation: u64,
        checkpoint: ContextCheckpointId,
        covered_through: MessageId,
        estimated_tokens: u64,
        compacted_at: Timestamp,
    ) -> bool {
        if prepared_generation != self.generation {
            return false;
        }

        self.active_checkpoint = Some(checkpoint);
        self.covered_through = Some(covered_through);
        self.last_estimated_tokens = Some(estimated_tokens);
        self.last_compacted_at = Some(compacted_at);
        true
    }

    pub fn pin(&mut self, item: ContextItemId) {
        if !self.pinned_items.contains(&item) {
            self.pinned_items.push(item);
            self.advance_generation();
        }
    }

    pub fn unpin(&mut self, item: ContextItemId) -> bool {
        let Some(index) = self
            .pinned_items
            .iter()
            .position(|candidate| *candidate == item)
        else {
            return false;
        };
        self.pinned_items.remove(index);
        self.advance_generation();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_checkpoint_cannot_replace_current_context() {
        let mut state = AgentContextState::default();
        let prepared_generation = state.generation;
        state.advance_generation();

        assert!(!state.accept_checkpoint(
            prepared_generation,
            ContextCheckpointId::new(),
            MessageId::new(),
            1_000,
            Timestamp::from_sequence(1),
        ));
        assert!(state.active_checkpoint.is_none());
    }

    #[test]
    fn current_checkpoint_updates_lineage_and_pressure() {
        let mut state = AgentContextState::default();
        let checkpoint = ContextCheckpointId::new();
        let message = MessageId::new();
        let compacted_at = Timestamp::from_sequence(2);

        assert!(state.accept_checkpoint(
            state.generation,
            checkpoint,
            message,
            2_500,
            compacted_at,
        ));
        assert_eq!(state.active_checkpoint, Some(checkpoint));
        assert_eq!(state.covered_through, Some(message));
        assert_eq!(state.last_estimated_tokens, Some(2_500));
        assert_eq!(state.last_compacted_at, Some(compacted_at));
    }

    #[test]
    fn pinning_is_deduplicated_and_changes_generation() {
        let mut state = AgentContextState::default();
        let item = ContextItemId::new();

        state.pin(item);
        state.pin(item);

        assert_eq!(state.pinned_items, vec![item]);
        assert_eq!(state.generation, 1);
        assert!(state.unpin(item));
        assert_eq!(state.generation, 2);
        assert!(!state.unpin(item));
    }
}
