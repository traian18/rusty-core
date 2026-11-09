//! Request-scoped publication adapter used by context observations.

use std::sync::Arc;

use async_trait::async_trait;
use rusty_protocol::events::AgentEvent;
use rusty_protocol::ids::RunId;
use tokio::sync::{mpsc, oneshot};

use crate::traits::RequestObservationPublisher;

pub(super) struct Publication {
    pub(super) event: AgentEvent,
    pub(super) run_id: RunId,
    pub(super) result: oneshot::Sender<bool>,
}

struct RunnerObservationPublisher(mpsc::UnboundedSender<Publication>);

#[async_trait]
impl RequestObservationPublisher for RunnerObservationPublisher {
    async fn publish(&self, event: AgentEvent, run_id: RunId) -> bool {
        let (result, receiver) = oneshot::channel();
        if self
            .0
            .send(Publication {
                event,
                run_id,
                result,
            })
            .is_err()
        {
            return false;
        }
        receiver.await.unwrap_or(false)
    }
}

pub(super) fn publisher(
    sender: mpsc::UnboundedSender<Publication>,
) -> Arc<dyn RequestObservationPublisher> {
    Arc::new(RunnerObservationPublisher(sender))
}
