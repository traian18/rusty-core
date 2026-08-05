//! Ordered event streaming for a session.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;
use harness_protocol::events::AgentEventEnvelope;
use tokio::sync::broadcast;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;

use crate::error::SdkError;

/// An ordered stream of [`AgentEventEnvelope`] values for one session.
///
/// Wraps the engine's internal `tokio::sync::broadcast::Receiver` so SDK
/// consumers depend only on this crate's public API, not on `tokio` channel
/// internals or `harness-runtime`.
///
/// If a consumer falls behind the broadcast buffer, the stream yields one
/// [`SdkError::Lagged`] item reporting how many events were skipped, then
/// resumes with the next available event. Only durable events survive a
/// lag or reconnect; ephemeral streaming deltas do not (see the workspace
/// README's "Durability and resume" section).
pub struct EventStream {
    inner: BroadcastStream<AgentEventEnvelope>,
}

impl EventStream {
    pub(crate) fn new(receiver: broadcast::Receiver<AgentEventEnvelope>) -> Self {
        Self {
            inner: BroadcastStream::new(receiver),
        }
    }

    /// Receive the next event, awaiting until one is available, or `None`
    /// once the session's broadcast sender has been dropped.
    pub async fn next(&mut self) -> Option<Result<AgentEventEnvelope, SdkError>> {
        StreamExt::next(&mut self.inner).await.map(map_item)
    }
}

fn map_item(
    item: Result<AgentEventEnvelope, BroadcastStreamRecvError>,
) -> Result<AgentEventEnvelope, SdkError> {
    item.map_err(|BroadcastStreamRecvError::Lagged(skipped)| SdkError::Lagged(skipped))
}

impl Stream for EventStream {
    type Item = Result<AgentEventEnvelope, SdkError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner)
            .poll_next(cx)
            .map(|opt| opt.map(map_item))
    }
}
