#![warn(clippy::all)]
//! Inference-only Copilot subscription adapter; tools belong to the harness.
pub mod auth;
mod backend;
mod catalog;
mod config;
pub mod credentials;
pub use backend::{api_root, GitHubCopilotBackend, GitHubCopilotFactory};
pub use config::GitHubCopilotConfig;
pub use harness_model::auth::InferenceAuth;
