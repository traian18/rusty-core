#![warn(clippy::all)]

//! Implements `ExecutionBackend` via `GenericModelBackend` and a
//! `ModelClient` for the OpenAI Chat Completions API.

pub mod backend;
pub mod client;
pub mod config;
pub mod usage;
pub mod wire;

pub use backend::{OpenAiBackend, OpenAiFactory};
pub use client::OpenAiClient;
pub use config::OpenAiConfig;
