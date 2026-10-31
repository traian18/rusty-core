//! Questions about a run that change nothing.

use crate::compiler::CompiledOrchestration;
use crate::definition::OrchestrationNodeId;

use super::lookup::transition_error;
use crate::state::{OrchestrationRunState, OrchestrationStatus, StepStatus, TransitionError};

/// Steps eligible for admission, in deterministic order (topological rank,
/// then node id). Concurrency is one, so at most one step is returned, and
/// none while paused, waiting, or already running.
pub fn ready_steps(
    compiled: &CompiledOrchestration,
    state: &OrchestrationRunState,
) -> Vec<OrchestrationNodeId> {
    if state.status != OrchestrationStatus::Ready || state.paused {
        return Vec::new();
    }
    let mut ready: Vec<_> = state
        .steps
        .iter()
        .filter_map(|(node_id, step)| (step.status == StepStatus::Ready).then_some(node_id.clone()))
        .collect();
    ready.sort_by(|left, right| {
        let rank = |id| {
            compiled
                .topological_rank
                .get(id)
                .copied()
                .unwrap_or(usize::MAX)
        };
        rank(left).cmp(&rank(right)).then_with(|| left.cmp(right))
    });
    ready.truncate(1);
    ready
}

/// Verify that a restored state belongs to exactly this compiled definition.
pub fn check_restorable(
    compiled: &CompiledOrchestration,
    state: &OrchestrationRunState,
) -> Result<(), TransitionError> {
    if state.definition_id != compiled.definition_id
        || state.definition_revision != compiled.revision
    {
        return Err(transition_error(
            "definition_mismatch",
            format!(
                "run was started on {}@{}, not {}@{}",
                state.definition_id,
                state.definition_revision,
                compiled.definition_id,
                compiled.revision
            ),
        ));
    }
    if state.definition_hash != compiled.content_hash {
        return Err(transition_error(
            "definition_hash_mismatch",
            "definition content changed since the run started",
        ));
    }
    if state.steps.len() != compiled.nodes.len()
        || !compiled.nodes.keys().all(|id| state.steps.contains_key(id))
    {
        return Err(transition_error(
            "definition_mismatch",
            "run step set does not match the definition",
        ));
    }
    Ok(())
}
