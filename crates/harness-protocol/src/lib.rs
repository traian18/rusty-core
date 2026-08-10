#![warn(clippy::all)]

//! Shared IDs, commands, events, and serializable protocol types; no runtime or I/O policy.

pub mod admission;
pub mod backend;
pub mod commands;
pub mod effects;
pub mod events;
pub mod ids;
pub mod lifecycle;
pub mod mcp;
pub mod messages;
pub mod rpc;
pub mod tools;
pub mod usage;
