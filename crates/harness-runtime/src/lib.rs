#![warn(clippy::all)]

//! Async execution of core effects, sessions, cancellation, and tool/backend coordination.
//!
//! Behavioral traits live in this crate while `harness-protocol` contains the
//! provider-neutral data exchanged across those traits.
//!
//! RC-300 (truthful persistence and recovery) wiring:
//!
//! - [`session_runtime`] — the authoritative [`SessionCommitter`] boundary,
//!   per-agent durable projections, and automatic/on-demand checkpoints.
//! - [`restore`] — host dependency resolution for restore (RC-304).
//! - [`session_manager`] — replay validation, snapshot migration, and strict
//!   dependency resolution during restore.

pub mod agent_runner;
pub mod agent_supervisor;
pub mod cancellation;
pub mod integration;
pub mod permissions;
pub mod resource_manager;
pub mod restore;
pub mod rpc;
pub mod scheduler;
pub mod session_client;
pub mod session_manager;
pub mod session_runtime;
pub mod spawn_tool;
pub mod traits;
pub mod workspace;

pub use integration::{IntegrationError, IntegrationFactory, IntegrationRegistry};

#[cfg(any(test, feature = "testing"))]
pub mod testing;
