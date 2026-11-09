//! What a request's observation reports to its chat: the final request's estimated input budget,
//! and the start and finish of each compaction operation that actually runs.
//!
//! Every event carries only identifiers, numbers and outcomes, never prompt, message or summary
//! text. Publication goes through the request's origin-bound publisher; a rejected publication is
//! not retried.

use rusty_protocol::backend::ExecutionRequest;
use rusty_protocol::events::{
    AgentEvent, CompactionOutcome, CompactionType, ContextAvailability, ContextUnavailableReason,
};
use rusty_protocol::ids::MessageId;

use crate::policy::{ContextDecision, ContextPolicy};
use crate::provider::ContextObservation;
use crate::providers::estimate_tokens;

/// The estimate for the request as it goes to the model. The window counts only when it belongs
/// to the request's own effective model.
pub(crate) fn context_update(
    request: &ExecutionRequest,
    observation: &ContextObservation,
    policy: ContextPolicy,
) -> AgentEvent {
    let model = request.params.model.clone();
    let availability = availability(request, observation, policy);
    AgentEvent::ContextUpdated {
        request_id: observation.origin.request_id,
        request_start_order: observation.request_start_order,
        model,
        availability,
        compaction_started: false,
        compaction_ended: false,
    }
}

fn availability(
    request: &ExecutionRequest,
    observation: &ContextObservation,
    policy: ContextPolicy,
) -> ContextAvailability {
    let unavailable = |reason| ContextAvailability::Unavailable { reason };
    let Some(trusted) = &observation.trusted_model_window else {
        return unavailable(ContextUnavailableReason::UnknownModelWindow);
    };
    // A request without its own model runs on a backend default nobody resolved: the window
    // cannot be trusted for it.
    if request.params.model.as_deref() != Some(trusted.model.as_str()) {
        return unavailable(ContextUnavailableReason::ModelWindowMismatch);
    }
    let estimate = estimate_tokens(request);
    match policy.evaluate(Some(trusted.model_window), estimate) {
        ContextDecision::Proceed { budget }
        | ContextDecision::ScheduleBackgroundCompaction { budget }
        | ContextDecision::CompactBeforeRequest { budget } => ContextAvailability::Available {
            estimated_input_tokens: estimate.tokens,
            input_budget: budget.input_budget,
            model_window: budget.context_window,
            remaining_input_tokens: budget.input_budget.saturating_sub(estimate.tokens),
            usage_percent: budget.pressure_percent,
        },
        ContextDecision::Unavailable { .. } => {
            unavailable(ContextUnavailableReason::UnusableInputBudget)
        }
    }
}

/// One compaction operation that has started: its finish is correlated by `operation_id`.
pub(crate) struct CompactionOperation {
    observation: ContextObservation,
    operation_id: String,
    compaction_type: CompactionType,
    before: Option<u64>,
}

impl CompactionOperation {
    /// Publishes the start of an operation whose work begins now. `None` when the start could not
    /// be published, so no finish without a start is published either.
    pub(crate) async fn start(
        observation: &ContextObservation,
        compaction_type: CompactionType,
        before: Option<u64>,
    ) -> Option<Self> {
        let operation = Self {
            observation: observation.clone(),
            operation_id: MessageId::new().to_string(),
            compaction_type,
            before: measured(compaction_type, before),
        };
        let started = AgentEvent::CompactionStarted {
            request_id: observation.origin.request_id,
            request_start_order: observation.request_start_order,
            operation_id: operation.operation_id.clone(),
            compaction_type,
            before_estimated_input_tokens: operation.before,
            compaction_started: true,
            compaction_ended: false,
        };
        observation
            .publisher
            .publish(started)
            .await
            .then_some(operation)
    }

    /// Publishes the operation's outcome. The after count is kept only for a successful
    /// standard compaction.
    pub(crate) async fn finish(self, outcome: CompactionOutcome, after: Option<u64>) -> bool {
        let after = match outcome {
            CompactionOutcome::Success => measured(self.compaction_type, after),
            CompactionOutcome::Failure | CompactionOutcome::Cancelled => None,
        };
        let finished = AgentEvent::CompactionFinished {
            request_id: self.observation.origin.request_id,
            request_start_order: self.observation.request_start_order,
            operation_id: self.operation_id,
            compaction_type: self.compaction_type,
            outcome,
            before_estimated_input_tokens: self.before,
            after_estimated_input_tokens: after,
            compaction_started: false,
            compaction_ended: true,
        };
        self.observation.publisher.publish(finished).await
    }
}

/// The outcome of synchronous compaction work that ran to its end: cancelled when the session was
/// cancelled meanwhile, so the request it compacted will not be sent.
pub(crate) fn outcome_of(observation: Option<&ContextObservation>) -> CompactionOutcome {
    if observation.is_some_and(|o| o.session_cancellation.is_cancelled()) {
        CompactionOutcome::Cancelled
    } else {
        CompactionOutcome::Success
    }
}

/// Background summaries report no measurements: their work does not change the request.
fn measured(compaction_type: CompactionType, value: Option<u64>) -> Option<u64> {
    match compaction_type {
        CompactionType::Standard => value,
        CompactionType::BackgroundSummary => None,
    }
}

#[cfg(test)]
mod tests;
