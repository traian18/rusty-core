#![warn(clippy::all)]
#![warn(clippy::undocumented_unsafe_blocks)]

//! Implements cancellable shell execution against the ToolExecutor contract.

pub mod executor;

pub use executor::{ExecInput, ExecTool};
