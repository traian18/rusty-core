#![warn(clippy::all)]

//! Implements ExecutionBackend using provider-neutral model machinery and a ModelClient.

pub mod backend;

/// Reusable test doubles and contract-test support for backend implementors.
///
/// Gated behind the `testing` feature (or an in-crate `cfg(test)` build) so
/// the fakes never reach a release binary. The crate declared the feature
/// but left this module unconditional, which meant `FakeModelClient` and the
/// contract suite compiled into every downstream build.
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use backend::{GenericModelBackend, RecoveryPolicy};
