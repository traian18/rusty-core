//! Provider-neutral model client trait.
//!
//! This module defines [`ModelClient`], the abstract interface that all model
//! providers (Anthropic, OpenAI, Gemini, etc.) must implement. The trait is
//! designed to be `dyn`-compatible so that implementations can be stored and
//! swapped as `Arc<dyn ModelClient>`.

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::events::{ModelError, ModelEvent, ModelResult};
use crate::request::{ModelCapabilities, ModelRequest};

/// The channel a [`ModelClient`] streams its events into.
///
/// A bounded `mpsc` rather than a `broadcast` channel on purpose: the
/// consumer relays every `TextDelta`/`ToolCallCompleted` into the agent
/// transcript, so an event that gets overwritten is lost model output. A
/// `broadcast` channel drops its oldest entries when a producer outruns the
/// consumer -- which a single HTTP chunk carrying a few hundred SSE frames
/// (a proxy flushing a buffered upstream all at once) does easily -- and
/// the consumer then only learns *how many* events it missed. `mpsc::send`
/// instead waits for capacity, so a burst slows the producer down rather
/// than being partially discarded.
pub type ModelEventSender = mpsc::Sender<ModelEvent>;

/// Delivers one event to the consumer, waiting for buffer capacity.
///
/// Returns [`ModelError::Cancelled`] if `cancel` fires while waiting, so a
/// client blocked on a full buffer still honors cancellation promptly. A
/// closed receiver is not an error: the consumer stopped listening, and the
/// client runs the stream to its end so the caller still gets a
/// [`ModelResult`] -- the same fail-open behavior clients had when the sink
/// was a `broadcast` channel with no subscribers.
pub async fn send_event(
    events: &ModelEventSender,
    event: ModelEvent,
    cancel: &CancellationToken,
) -> Result<(), ModelError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(ModelError::Cancelled),
        sent = events.send(event) => {
            let _ = sent;
            Ok(())
        }
    }
}

/// A provider-neutral model client.
///
/// Implementations wrap a specific LLM provider (Anthropic, OpenAI, Gemini,
/// etc.) and are responsible for:
///
/// * Serializing the [`ModelRequest`] into the provider's wire format.
/// * Streaming back normalized [`ModelEvent`]s (text deltas, reasoning,
///   tool call fragments, usage updates, final result, or errors).
/// * Observing the [`CancellationToken`] so that in-flight requests can be
///   aborted promptly.
///
/// # Object safety
///
/// This trait is `dyn`-compatible — all methods take `&self` and return owned
/// values or futures. Use `Arc<dyn ModelClient>` to share a client across
/// threads.
#[async_trait]
pub trait ModelClient: Send + Sync {
    /// Returns the capabilities supported by this model provider.
    ///
    /// This method is synchronous because capabilities are known statically
    /// at construction time and never change during the lifetime of the client.
    fn capabilities(&self) -> ModelCapabilities;

    /// Execute a streaming request against the model.
    ///
    /// The implementation streams [`ModelEvent`]s into `events` as they occur
    /// and returns the final [`ModelResult`] once execution completes
    /// (or a [`ModelError`] if something went wrong).
    ///
    /// `events` is bounded and applies backpressure: send through
    /// [`send_event`] (or await `events.send` directly) rather than
    /// `try_send`, so no event is ever dropped when the consumer is briefly
    /// behind.
    ///
    /// # Cancellation
    ///
    /// When `cancel` is triggered the implementation should stop work as soon
    /// as practical and return [`ModelError::Cancelled`].
    async fn stream(
        &self,
        request: ModelRequest,
        events: ModelEventSender,
        cancel: CancellationToken,
    ) -> Result<ModelResult, ModelError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn send_event_waits_for_capacity_instead_of_dropping() {
        let (tx, mut rx) = mpsc::channel(1);
        let cancel = CancellationToken::new();
        send_event(&tx, ModelEvent::TextDelta { delta: "a".into() }, &cancel)
            .await
            .expect("first send fits");

        let producer = tokio::spawn({
            let tx = tx.clone();
            let cancel = cancel.clone();
            async move { send_event(&tx, ModelEvent::TextDelta { delta: "b".into() }, &cancel).await }
        });
        tokio::task::yield_now().await;
        assert!(
            !producer.is_finished(),
            "second send must wait for the consumer"
        );

        let first = rx.recv().await.expect("buffered event");
        assert!(matches!(first, ModelEvent::TextDelta { delta } if delta == "a"));
        producer
            .await
            .expect("join")
            .expect("send succeeds once space frees up");
        let second = rx.recv().await.expect("second event");
        assert!(matches!(second, ModelEvent::TextDelta { delta } if delta == "b"));
    }

    #[tokio::test]
    async fn send_event_honors_cancellation_while_blocked() {
        let (tx, _rx) = mpsc::channel(1);
        let cancel = CancellationToken::new();
        send_event(&tx, ModelEvent::TextDelta { delta: "a".into() }, &cancel)
            .await
            .expect("fills the buffer");
        cancel.cancel();
        let result = send_event(&tx, ModelEvent::TextDelta { delta: "b".into() }, &cancel).await;
        assert!(matches!(result, Err(ModelError::Cancelled)));
    }

    #[tokio::test]
    async fn send_event_treats_a_closed_receiver_as_fail_open() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let result = send_event(
            &tx,
            ModelEvent::TextDelta { delta: "a".into() },
            &CancellationToken::new(),
        )
        .await;
        assert!(result.is_ok());
    }
}
