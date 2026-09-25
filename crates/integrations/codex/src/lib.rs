#![warn(clippy::all)]
//! Inference-only ChatGPT subscription adapter. The harness owns the tool loop.
pub mod auth;
pub mod backend;
pub mod config;
pub use backend::{CodexBackend, CodexFactory};
pub use config::CodexConfig;
