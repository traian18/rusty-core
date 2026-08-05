#![warn(clippy::all)]

//! Deterministic Agent/Session domain semantics: state, transitions, commands, and effects. No I/O.

pub mod agent;
pub mod agent_state;
pub mod budget;
pub mod capabilities;
pub mod context_state;
pub mod transcript;
pub mod transitions;
pub mod usage;
