//! Saving a step's progress so an interrupted attempt can resume.

use serde_json::Value;

use super::lookup::active_attempt_mut;
use crate::{
    compiler::CompiledOrchestration,
    definition::OrchestrationNodeId,
    state::{OrchestrationEffect, OrchestrationEvent, OrchestrationRunState, TransitionError},
};

/// Stores `value` as the attempt's checkpoint and reports task progress from it: how many tasks
/// are done out of the plan's total.
pub(super) fn record_checkpoint(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node_id: OrchestrationNodeId,
    attempt: u32,
    value: Value,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    let completed = value["completed"].as_array().map_or(0, Vec::len);
    let total = value["plan"]["tasks"].as_array().map_or(0, Vec::len);
    active_attempt_mut(compiled, state, &node_id, attempt)?.checkpoint = Some(value);
    Ok(vec![OrchestrationEffect::Emit(
        OrchestrationEvent::TaskProgress {
            node_id,
            attempt,
            completed,
            total,
        },
    )])
}
