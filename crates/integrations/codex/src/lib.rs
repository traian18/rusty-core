#![warn(clippy::all)]

//! Implements a specialized `ExecutionBackend` for Codex: drives the
//! `codex` CLI as a subprocess rather than calling a model API directly.
//! See `crates/integrations/codex/PLAN.md` for the design rationale and
//! `wire.rs` for the verified wire schema.

pub mod backend;
pub mod config;
pub mod wire;

pub use backend::{CodexBackend, CodexFactory};
pub use config::CodexConfig;
