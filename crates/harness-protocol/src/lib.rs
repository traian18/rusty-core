#![warn(clippy::all)]

//! Shared IDs, commands, events, and serializable protocol types; no runtime or I/O policy.

pub mod backend;
pub mod commands;
pub mod effects;
pub mod events;
pub mod ids;
pub mod messages;
pub mod tools;
pub mod usage;
