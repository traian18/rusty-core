#![warn(clippy::all)]

//! The provider-neutral model vocabulary (requests, events, errors, the `ModelClient` trait) and
//! `GenericModelBackend`, which turns any `ModelClient` into an `ExecutionBackend` with retries,
//! idle timeouts and a circuit breaker.

pub mod auth;
pub mod backend;
pub mod client;
pub mod duration_serde;
pub mod events;
pub mod provider_options;
pub mod request;
pub mod retry;

/// Reusable test doubles and contract-test support for backend implementors.
///
/// Gated behind the `testing` feature (or an in-crate `cfg(test)` build) so
/// the fakes never reach a release binary. The crate declared the feature
/// but left this module unconditional, which meant `FakeModelClient` and the
/// contract suite compiled into every downstream build.
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use backend::{GenericModelBackend, RecoveryPolicy};
pub use client::{send_event, ModelClient, ModelEventSender};
pub use events::{ModelError, ModelEvent, ModelResult};
pub use provider_options::merge_provider_options;
pub use request::{ModelCapabilities, ModelRequest};
pub use retry::parse_retry_after;
