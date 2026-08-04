#![warn(clippy::all)]

//! Implements ExecutionBackend using provider-neutral model machinery and a ModelClient.

pub mod backend;

/// Reusable test doubles and contract-test support for backend implementors.
pub mod testing;

pub use backend::{GenericModelBackend, RecoveryPolicy};
