#![warn(clippy::all)]

//! Implements ExecutionBackend via GenericModelBackend and a ModelClient for the Anthropic Messages API. Empty until Phase 3.

pub mod config;
pub mod wire;
pub mod client;
pub mod usage;
pub mod backend;

pub use config::AnthropicConfig;
pub use backend::{AnthropicBackend, AnthropicFactory};
