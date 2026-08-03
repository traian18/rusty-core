#![warn(clippy::all)]

//! Implements a specialized `ExecutionBackend` for Claude Code: drives the
//! `claude` CLI as a subprocess rather than calling a model API directly.
//! See `crates/integrations/claude-code/PLAN.md` for the design rationale.

pub mod backend;
pub mod config;
pub mod wire;

pub use backend::{ClaudeCodeBackend, ClaudeCodeFactory};
pub use config::ClaudeCodeConfig;
