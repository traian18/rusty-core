#![forbid(unsafe_code)]
//! Deterministic Agent/Session domain semantics: state, transitions, commands, and effects. No I/O.

pub mod agent;
pub mod agent_state;
pub mod behavior;
pub mod budget;
pub mod capabilities;
mod content_hash;
pub mod context_state;
pub mod execution_policy;
pub mod orchestration;
pub mod transcript;
pub mod transitions;
pub mod usage;
