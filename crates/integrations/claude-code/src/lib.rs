#![warn(clippy::all)]

//! Implements ExecutionBackend by wrapping the Claude Code CLI as a subprocess.
//!
//! Unlike other integrations that call a model provider's HTTP API directly,
//! this integration runs the Claude Code CLI (`claude`) as a child process,
//! captures its structured stream-json output, and translates it into harness
//! ExecutionEvent streams. The CLI itself already manages authentication via
//! local system credentials, tool use, and context management.

pub mod config;
pub mod executor;
pub mod backend;

pub use config::ClaudeCodeConfig;
pub use backend::{ClaudeCodeBackend, ClaudeCodeFactory};
