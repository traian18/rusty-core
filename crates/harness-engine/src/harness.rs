//! Top-level [`Harness`] entry point — the public API root.
//!
//! `Harness` is a zero‑state factory that creates [`SessionBuilder`]s.
//! All session configuration (backend, tool registry, etc.) is done
//! through the builder.

use crate::session_builder::SessionBuilder;

/// The top-level entry point for creating harness sessions.
///
/// This is a stateless singleton.  Call [`Harness::new()`] to get an
/// instance, then chain `.session().backend(...).tools(...).start().await?`
/// to create a live session.
pub struct Harness;

impl Harness {
    /// Create a new harness instance.
    ///
    /// Returns a zero‑state `Harness`.  All configuration happens on the
    /// [`SessionBuilder`] obtained via [`session()`](Harness::session).
    pub fn new() -> Self {
        Self
    }

    /// Begin building a new session.
    ///
    /// Returns a [`SessionBuilder`] that must be configured with
    /// [`backend()`](SessionBuilder::backend) and
    /// [`tools()`](SessionBuilder::tools) before calling
    /// [`start()`](SessionBuilder::start).
    pub fn session(&self) -> SessionBuilder {
        SessionBuilder::new()
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}
