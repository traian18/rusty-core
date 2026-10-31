#![warn(clippy::all)]

//! Implements `ExecutionBackend` via `GenericModelBackend` and a
//! `ModelClient` for the Gemini API.

pub mod backend;
pub mod client;
pub mod config;
pub mod usage;
pub mod wire;

pub use backend::{GeminiBackend, GeminiFactory};
pub use config::GeminiConfig;
