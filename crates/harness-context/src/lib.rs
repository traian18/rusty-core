#![warn(clippy::all)]

//! Context-provider abstractions, model-aware budgeting, and context assembly.
//!
//! Canonical conversation history remains owned by the core. This crate
//! prepares bounded inference views and decides when compaction is required.

pub mod backend;
pub mod policy;
pub mod provider;
pub mod providers;

pub use backend::ContextAssemblingBackend;
pub use policy::{
    ContextBudget, ContextBudgetUnavailable, ContextDecision, ContextOwnership, ContextPolicy,
    ContextPolicyError, TokenEstimate,
};
pub use provider::ContextProvider;
pub use providers::{
    ChainedContextProvider, CompactionRecord, PolicyDrivenCompactionProvider,
    StaticSystemPromptProvider, TruncatingCompactionProvider, WorkspaceInfoProvider,
};
