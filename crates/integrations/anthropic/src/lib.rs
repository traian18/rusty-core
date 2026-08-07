#![warn(clippy::all)]

//! Implements ExecutionBackend via GenericModelBackend and a ModelClient for the Anthropic Messages API. Empty until Phase 3.

pub mod backend;
pub mod client;
pub mod config;
pub mod usage;
pub mod wire;

pub use backend::{AnthropicBackend, AnthropicFactory};
pub use config::AnthropicConfig;
