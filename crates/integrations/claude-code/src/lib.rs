#![warn(clippy::all)]
//! Inference-only Claude subscription integration. Tool execution belongs to the harness.
pub mod auth;
pub mod backend;
pub mod config;
pub use backend::{ClaudeCodeBackend, ClaudeCodeFactory};
pub use config::ClaudeCodeConfig;
