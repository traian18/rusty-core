//! Deterministic orchestration definitions, compilation, and run-state transitions.
//!
//! This module contains no I/O. Runtime adapters execute emitted effects and feed
//! their outcomes back as commands, allowing future node implementations and UI
//! editors to share the same stable graph contract.

mod compiler;
mod default;
mod definition;
mod registry;
mod state;

pub use compiler::{
    compile, CompiledOrchestration, DefinitionIssue, DefinitionValidationError,
    SUPPORTED_SCHEMA_KEYWORDS,
};
pub use default::default_orchestration_definition;
pub use definition::*;
pub use registry::{DefinitionRef, DefinitionRegistry, RegistryError};
pub use state::*;

#[cfg(test)]
mod tests;
