//! Test doubles for request observations: a publisher that records what it is given, and an
//! observation bound to a fresh origin.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusty_protocol::events::{AgentEvent, CompactionOutcome, CompactionType};
use rusty_protocol::ids::{AgentId, RequestId, RunId, SessionId};
use tokio_util::sync::CancellationToken;

use crate::provider::{
    ContextObservation, ContextObservationOrigin, ContextObservationPublisher, TrustedModelWindow,
};

/// Records every published event; rejects them all when `accept` is false.
pub(crate) struct RecordingPublisher {
    pub(crate) events: Mutex<Vec<AgentEvent>>,
    pub(crate) accept: bool,
}

impl RecordingPublisher {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            accept: true,
        })
    }

    pub(crate) fn events(&self) -> Vec<AgentEvent> {
        self.events.lock().unwrap().clone()
    }

    /// The lifecycle events as `(started, type, outcome, operation id)`, in publication order.
    pub(crate) fn lifecycle(
        &self,
    ) -> Vec<(bool, CompactionType, Option<CompactionOutcome>, String)> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                AgentEvent::CompactionStarted {
                    compaction_type,
                    operation_id,
                    ..
                } => Some((true, compaction_type, None, operation_id)),
                AgentEvent::CompactionFinished {
                    compaction_type,
                    outcome,
                    operation_id,
                    ..
                } => Some((false, compaction_type, Some(outcome), operation_id)),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl ContextObservationPublisher for RecordingPublisher {
    async fn publish(&self, event: AgentEvent) -> bool {
        if self.accept {
            self.events.lock().unwrap().push(event);
        }
        self.accept
    }
}

/// An observation for request `request_id`, with `window` as the trusted window of `model`.
pub(crate) fn observation(
    request_id: RequestId,
    window: Option<(&str, u64)>,
    publisher: Arc<RecordingPublisher>,
) -> ContextObservation {
    ContextObservation {
        origin: ContextObservationOrigin {
            session_id: SessionId::new(),
            agent_id: AgentId::new(),
            parent_agent_id: None,
            run_id: RunId::new(),
            request_id,
        },
        request_start_order: 3,
        trusted_model_window: window.map(|(model, model_window)| TrustedModelWindow {
            model: model.into(),
            model_window,
        }),
        session_cancellation: CancellationToken::new(),
        publisher,
    }
}
