#![warn(clippy::all)]

//! Async execution of core effects, sessions, cancellation, and tool/backend coordination.
//!
//! # Design boundary
//!
//! The **behavioral traits** (e.g. [`ExecutionBackend`], [`ToolExecutor`],
//! [`ToolRegistry`], [`EventSink`], [`Workspace`]) live **here** in
//! `harness-runtime`. The sister crate [`harness-protocol`] holds only the
//! **wire data types** they exchange — [`ExecutionRequest`],
//! [`ExecutionEvent`], [`ToolCall`], [`ToolResult`], [`AgentEventEnvelope`],
//! and so on.
//!
//! This separation means `harness-protocol` has zero knowledge of async I/O,
//! cancellation, or runtime policies — it is purely a schema crate.  All
//! runtime semantics are defined in this crate's traits module.

pub mod agent_runner;
pub mod traits;
pub mod cancellation;
pub mod session_runtime;
pub mod session_client;
pub mod workspace;

#[cfg(any(test, feature = "testing"))]
pub mod testing;
