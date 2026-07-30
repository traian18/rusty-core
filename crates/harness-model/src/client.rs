//! Provider-neutral model client trait.
//!
//! This module defines [`ModelClient`], the abstract interface that all model
//! providers (Anthropic, OpenAI, Gemini, etc.) must implement. The trait is
//! designed to be `dyn`-compatible so that implementations can be stored and
//! swapped as `Arc<dyn ModelClient>`.

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::events::{ModelError, ModelEvent, ModelResult};
use crate::request::{ModelCapabilities, ModelRequest};

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
    /// # Cancellation
    ///
    /// When `cancel` is triggered the implementation should stop work as soon
    /// as practical and return [`ModelError::Cancelled`].
    async fn stream(
        &self,
        request: ModelRequest,
        events: broadcast::Sender<ModelEvent>,
        cancel: CancellationToken,
    ) -> Result<ModelResult, ModelError>;
}
