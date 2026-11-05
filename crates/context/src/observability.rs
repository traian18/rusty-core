//! Optional, non-blocking context telemetry. Events never contain message or summary text.
//!
//! Subscribe before assembling requests. Consumers must handle broadcast lag and keep
//! their subscription alive beyond a model turn to receive background summary events.

use rusty_protocol::backend::ExecutionRequest;
use rusty_protocol::ids::MessageId;
use tokio::sync::broadcast;

use crate::policy::{ContextDecision, ContextPolicy};
use crate::providers::estimate_tokens;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasurementStage {
    BeforeCompaction,
    AfterCompaction,
}

/// An assembly-time estimate, not cumulative usage or a next-turn projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMeasurement {
    pub conversation_id: Option<MessageId>,
    pub stage: MeasurementStage,
    pub estimated_input_tokens: u64,
    pub exact: bool,
    pub context_window: Option<u64>,
    pub input_budget: Option<u64>,
    pub pressure_percent: Option<u16>,
    pub soft_limit_percent: u8,
    pub hard_limit_percent: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionMode {
    Standard,
    Smart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionPhase {
    Started,
    Judging {
        candidates: usize,
    },
    Judged {
        essential: usize,
        useful: usize,
        disposable: usize,
    },
    /// Judge unavailable; unclassified candidates are treated as useful.
    JudgeFailed,
    Summarizing {
        messages: usize,
    },
    Ready,
    Applied,
    Completed,
    NoChange,
    /// Summary generation failed. No raw provider error is exposed.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionUpdate {
    pub conversation_id: Option<MessageId>,
    pub operation_id: MessageId,
    pub mode: CompactionMode,
    pub phase: CompactionPhase,
    pub before_tokens: Option<u64>,
    pub after_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextEvent {
    Measured(ContextMeasurement),
    Compaction(CompactionUpdate),
}

/// A bounded broadcast sink; a slow or absent listener never delays inference.
#[derive(Clone)]
pub struct ContextObserver {
    sender: broadcast::Sender<ContextEvent>,
}

impl ContextObserver {
    /// `capacity` must be greater than zero (as required by tokio broadcast).
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ContextEvent> {
        self.sender.subscribe()
    }

    pub(crate) fn emit(&self, event: ContextEvent) {
        let _ = self.sender.send(event);
    }

    pub(crate) fn measure(
        &self,
        request: &ExecutionRequest,
        policy: ContextPolicy,
        window: Option<u64>,
        stage: MeasurementStage,
    ) {
        let estimate = estimate_tokens(request);
        let budget = match policy.evaluate(window, estimate) {
            ContextDecision::Proceed { budget }
            | ContextDecision::ScheduleBackgroundCompaction { budget }
            | ContextDecision::CompactBeforeRequest { budget } => Some(budget),
            ContextDecision::Unavailable { .. } => None,
        };
        self.emit(ContextEvent::Measured(ContextMeasurement {
            conversation_id: request.messages.first().map(|m| m.id),
            stage,
            estimated_input_tokens: estimate.tokens,
            exact: estimate.exact,
            context_window: window,
            input_budget: budget.map(|b| b.input_budget),
            pressure_percent: budget.map(|b| b.pressure_percent),
            soft_limit_percent: policy.soft_limit_percent,
            hard_limit_percent: policy.hard_limit_percent,
        }));
    }
}

#[derive(Clone)]
pub(crate) struct CompactionTelemetry {
    pub observer: Option<ContextObserver>,
    pub conversation_id: Option<MessageId>,
    pub operation_id: MessageId,
    pub mode: CompactionMode,
}

impl CompactionTelemetry {
    pub fn update(
        &self,
        phase: CompactionPhase,
        before_tokens: Option<u64>,
        after_tokens: Option<u64>,
    ) {
        if let Some(observer) = &self.observer {
            observer.emit(ContextEvent::Compaction(CompactionUpdate {
                conversation_id: self.conversation_id,
                operation_id: self.operation_id,
                mode: self.mode,
                phase,
                before_tokens,
                after_tokens,
            }));
        }
    }
}
