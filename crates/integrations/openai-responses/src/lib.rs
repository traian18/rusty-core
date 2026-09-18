#![warn(clippy::all)]

//! Implements `ExecutionBackend` via `GenericModelBackend` and a
//! `ModelClient` for the OpenAI Responses API (`{base_url}/responses`) --
//! distinct from `harness-integration-openai`, which speaks the older Chat
//! Completions shape.

pub mod backend;
pub mod client;
pub mod config;
pub mod usage;
pub mod wire;

pub use backend::{OpenAiResponsesBackend, OpenAiResponsesFactory};
pub use client::OpenAiResponsesClient;
pub use config::OpenAiResponsesConfig;
