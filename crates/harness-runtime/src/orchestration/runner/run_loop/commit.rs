//! Applying a command: reduce, commit, publish, then dispatch — in that order.

use harness_core::orchestration::{
    apply, OrchestrationCommand, OrchestrationEffect, OrchestrationNodeId, OrchestrationResult,
    OrchestrationStatus,
};

use super::super::super::store::{
    OrchestrationEventEnvelope, OrchestrationSnapshot, OrchestrationStoreError,
};
use super::super::{now_ms, OrchestrationRuntimeError, OrchestrationUpdate};
use super::{ApplyError, RunLoop};

/// The reducer's effects for one command, sorted by what happens to them.
#[derive(Default)]
struct EffectBatch {
    envelopes: Vec<OrchestrationEventEnvelope>,
    last_sequence: u64,
    finish: Option<OrchestrationResult>,
    execute: Option<(OrchestrationNodeId, u32)>,
    cancels: Vec<(OrchestrationNodeId, u32)>,
}

impl RunLoop {
    pub(super) async fn apply(
        &mut self,
        command: OrchestrationCommand,
    ) -> Result<(), OrchestrationRuntimeError> {
        self.try_apply(command).await.map_err(Into::into)
    }

    /// Reduce, commit, publish, then dispatch — in that order.
    pub(super) async fn try_apply(
        &mut self,
        command: OrchestrationCommand,
    ) -> Result<(), ApplyError> {
        let effects =
            apply(&self.runner.compiled, &mut self.state, command).map_err(ApplyError::Rejected)?;
        self.clock
            .track_input_wait(self.state.status == OrchestrationStatus::WaitingForInput);
        let batch = self.collect_effects(effects);
        self.persist(&batch)
            .await
            .map_err(|error| ApplyError::Fatal(error.into()))?;
        self.last_sequence = batch.last_sequence;
        self.publish_committed(batch.envelopes);
        self.cancel_matching(&batch.cancels);
        if let Some(result) = batch.finish {
            self.result = Some(result);
        }
        if let Some((node_id, attempt)) = batch.execute {
            self.dispatch(node_id, attempt);
        }
        Ok(())
    }

    fn collect_effects(&self, effects: Vec<OrchestrationEffect>) -> EffectBatch {
        let timestamp_ms = now_ms();
        let mut batch = EffectBatch {
            last_sequence: self.last_sequence,
            ..EffectBatch::default()
        };
        for effect in effects {
            match effect {
                OrchestrationEffect::Emit(event) => {
                    batch.last_sequence += 1;
                    batch.envelopes.push(OrchestrationEventEnvelope {
                        run_id: self.state.run_id.clone(),
                        definition_id: self.state.definition_id.clone(),
                        revision: self.state.definition_revision,
                        sequence: batch.last_sequence,
                        timestamp_ms,
                        event,
                    });
                }
                OrchestrationEffect::Finish(result) => batch.finish = Some(result),
                OrchestrationEffect::ExecuteStep { node_id, attempt } => {
                    batch.execute = Some((node_id, attempt))
                }
                OrchestrationEffect::CancelStep { node_id, attempt } => {
                    batch.cancels.push((node_id, attempt))
                }
            }
        }
        batch
    }

    /// Commits events and the snapshot together, before anyone sees them.
    async fn persist(&self, batch: &EffectBatch) -> Result<(), OrchestrationStoreError> {
        let Some(store) = &self.runner.store else {
            return Ok(());
        };
        let snapshot = OrchestrationSnapshot {
            state: self.state.clone(),
            last_sequence: batch.last_sequence,
            elapsed_ms: self.clock.elapsed_ms(),
            result: batch.finish.clone(),
        };
        store.commit(&batch.envelopes, &snapshot).await
    }

    fn publish_committed(&mut self, envelopes: Vec<OrchestrationEventEnvelope>) {
        for envelope in envelopes {
            self.publish(OrchestrationUpdate::Event(envelope.clone()));
            self.log.push(envelope);
        }
        self.state_tx.send_replace(self.state.clone());
    }

    fn cancel_matching(&self, cancels: &[(OrchestrationNodeId, u32)]) {
        let Some(active) = &self.active else {
            return;
        };
        let cancelled = cancels
            .iter()
            .any(|(node_id, attempt)| active.node_id == *node_id && active.attempt == *attempt);
        if cancelled {
            active.token.cancel();
        }
    }
}
