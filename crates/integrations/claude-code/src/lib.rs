#![warn(clippy::all)]

//! Implements ExecutionBackend by wrapping the Claude Code CLI as a subprocess.
//!
//! Unlike other integrations that call a model provider's HTTP API directly,
//! this integration runs the Claude Code CLI (`claude`) as a child process,
//! captures its structured stream-json output, and translates it into harness
//! ExecutionEvent streams. The CLI itself already manages authentication via
//! local system credentials, tool use, and context management. See
//! `PLAN.md` for the design rationale and `wire.rs` for the verified wire
//! schema (mirrors `harness-integration-codex`'s shape).

pub mod backend;
pub mod config;
pub mod wire;

pub use backend::{ClaudeCodeBackend, ClaudeCodeFactory};
pub use config::ClaudeCodeConfig;
