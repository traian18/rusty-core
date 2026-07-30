//! Test doubles for the runtime's behavioral traits.
//!
//! These fakes are only compiled for tests or when the `testing` feature is
//! enabled, allowing other crates to depend on `harness-runtime` with
//! `features = ["testing"]` to reuse them in their own test suites.

#[cfg(any(test, feature = "testing"))]
pub mod fake_backend;

#[cfg(any(test, feature = "testing"))]
pub mod fake_tools;

#[cfg(any(test, feature = "testing"))]
pub use fake_backend::FakeBackend;

#[cfg(any(test, feature = "testing"))]
pub use fake_tools::{FakeToolExecutor, FakeToolRegistry};
