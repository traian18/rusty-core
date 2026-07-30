#![warn(clippy::all)]

//! Implements cancellable shell execution against the ToolExecutor contract.

pub mod executor;

pub use executor::{ExecInput, ExecTool};
