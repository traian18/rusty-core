//! Session lifecycle management, multi-session tracking, and durable restoration.
//!
//! This module coordinates runtime session lifecycles, active session tracking,
//! scheduler admission, background task supervision, and truthful session restoration.
//!
//! # Architecture & SOLID Principles
//!
//! - **Contracts & Interfaces ([`contracts`])**: Behavioral traits separating concerns:
//!   - [`SessionRegistry`]: Active session querying and event subscriptions.
//!   - [`SessionLifecycle`]: Session creation, shutdown, and timeout-bounded bulk drain.
//!   - [`SessionRestorer`]: Stateful session restoration without replaying side effects.
//!   - [`SessionService`]: Composite trait combining all capabilities.
//! - **Active Registry ([`registry`])**: Thread-safe in-memory session and permit tracking.
//! - **Task Supervision ([`supervisor`])**: Failure detection and state propagation for root agent tasks.
//! - **Restoration Engine ([`restorer`])**: Replay validation, schema migration, and backend reconstruction.
//! - **Session Manager ([`manager`])**: Unified coordinator implementing the contracts.
//! - **Errors ([`errors`])**: Domain-specific error variants.

pub mod contracts;
pub mod errors;
pub mod manager;
pub mod registry;
pub mod restorer;
pub mod supervisor;

pub use contracts::{SessionLifecycle, SessionRegistry, SessionRestorer, SessionService};
pub use errors::SessionManagerError;
pub use manager::SessionManager;
pub use registry::ActiveSessionRegistry;
pub use restorer::{backend_reference_key, SessionRestorerEngine};
pub use supervisor::SessionSupervisor;

#[cfg(test)]
mod tests;
