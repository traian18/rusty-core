#![warn(clippy::all)]

//! GitHub Copilot CLI execution adapter.
//!
//! Authentication and credential persistence remain owned by the official
//! `copilot` CLI. The adapter only invokes its documented programmatic JSON
//! mode and normalizes the result into harness execution events.

mod backend;
mod config;
mod wire;

pub use backend::{GitHubCopilotBackend, GitHubCopilotFactory};
pub use config::GitHubCopilotConfig;
