#![warn(clippy::all)]

//! Async execution of core effects, sessions, cancellation, and tool/backend coordination.
//!
//! Behavioral traits live in this crate while `harness-protocol` contains the
//! provider-neutral data exchanged across those traits.

pub mod agent_runner;
pub mod cancellation;
pub mod integration;
pub mod permissions;
pub mod session_client;
pub mod session_runtime;
pub mod traits;
pub mod workspace;

pub use integration::{IntegrationError, IntegrationFactory, IntegrationRegistry};

#[cfg(any(test, feature = "testing"))]
pub mod testing;
