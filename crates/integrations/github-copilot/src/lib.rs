#![warn(clippy::all)]
//! Inference-only Copilot subscription adapter; tools belong to the harness.
pub mod auth;
mod backend;
mod config;
pub use backend::{GitHubCopilotBackend, GitHubCopilotFactory};
pub use config::GitHubCopilotConfig;
