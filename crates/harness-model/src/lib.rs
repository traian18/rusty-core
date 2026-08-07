#![warn(clippy::all)]

//! Provider-neutral model requests, responses, streams, tool calls, and usage types.

pub mod client;
pub mod events;
pub mod provider_options;
pub mod request;
pub mod retry;

pub use client::ModelClient;
pub use events::{ModelError, ModelEvent, ModelResult};
pub use provider_options::merge_provider_options;
pub use request::{ModelCapabilities, ModelRequest};
pub use retry::parse_retry_after;
