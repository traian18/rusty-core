//! Test doubles for anything that needs a [`SessionStore`], exported for embedding crates.
//!
//! - [`MemoryStore`] is an in-memory store with the same contract as the real ones (duplicate
//!   sequences rejected, snapshots replaced, reads cut off at the snapshot) plus scripted append
//!   and snapshot failures, for exercising durability policies.
//! - [`FaultInjectingStore`] wraps any store and fails chosen calls ("the 3rd append", "every
//!   snapshot"), so crash and restart tests run against the real persistence path and then
//!   "restart" on the same data with the faults cleared.

mod fault_injecting;
mod memory;

pub use fault_injecting::FaultInjectingStore;
pub use memory::{test_snapshot, MemoryStore};

#[cfg(test)]
mod tests;
