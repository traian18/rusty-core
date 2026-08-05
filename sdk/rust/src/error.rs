//! Error types returned by the SDK facade.

use thiserror::Error;

/// Unified error type for every fallible [`crate::Client`]/[`crate::Session`]
/// operation.
///
/// This wraps the underlying engine and store errors so applications can
/// depend on one error enum from this crate instead of matching on internal
/// engine crate errors directly. Variants are additive: new variants may be
/// added in minor SDK releases, so callers should not exhaustively match
/// without a wildcard arm.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SdkError {
    /// An error surfaced by the embedded harness engine (unknown provider,
    /// invalid integration config, session/runtime failure, store failure).
    #[error(transparent)]
    Engine(#[from] harness_engine::HarnessError),

    /// An error surfaced directly by the configured session store.
    #[error(transparent)]
    Store(#[from] harness_session_store::store::StoreError),

    /// The event stream consumer fell behind the broadcast buffer and lost
    /// `count` events. The stream resumes with the next available event;
    /// callers that need a gap-free history should use durable event replay
    /// (`Client::list_sessions` / session restore) instead of the live
    /// stream alone.
    #[error("event stream lagged behind by {0} event(s); some events were dropped")]
    Lagged(u64),
}
