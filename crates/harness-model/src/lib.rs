#![warn(clippy::all)]

//! Provider-neutral model requests, responses, streams, tool calls, and usage types.

pub mod client;
pub mod events;
pub mod request;

pub use client::ModelClient;
pub use events::{ModelError, ModelEvent, ModelResult};
pub use request::{ModelCapabilities, ModelRequest};
