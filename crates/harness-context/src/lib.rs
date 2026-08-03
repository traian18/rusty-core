#![warn(clippy::all)]

//! Context-provider abstractions and context assembly.
//!
//! See [`provider`] for why this lives as a backend decorator rather than
//! inside `harness-core`'s pure state machine.

pub mod backend;
pub mod provider;
pub mod providers;

pub use backend::ContextAssemblingBackend;
pub use provider::ContextProvider;
pub use providers::{
    ChainedContextProvider, StaticSystemPromptProvider, TruncatingCompactionProvider,
    WorkspaceInfoProvider,
};
