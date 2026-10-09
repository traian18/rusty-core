#![warn(clippy::all)]

//! Context-provider abstractions, model-aware budgeting, and context assembly.
//!
//! Canonical conversation history remains owned by the core. This crate
//! prepares bounded inference views and decides when compaction is required.

pub mod backend;
pub mod compaction;
pub mod importance;
pub mod policy;
pub mod provider;
pub mod providers;
pub mod summary;

pub use backend::ContextAssemblingBackend;
#[cfg(feature = "jev-http")]
pub use importance::OpenRouterDecisions;
pub use importance::{
    DecisionsTransport, Importance, ImportanceJudge, JevImportanceJudge, JudgeError, JudgeItem,
    Verdict,
};
pub use policy::{
    ContextBudget, ContextBudgetUnavailable, ContextDecision, ContextOwnership, ContextPolicy,
    ContextPolicyError, TokenEstimate,
};
pub use provider::ContextProvider;
pub use providers::{
    ChainedContextProvider, CompactionRecord, PolicyDrivenCompactionProvider,
    StaticSystemPromptProvider, TruncatingCompactionProvider, WorkspaceInfoProvider,
};
pub use summary::{
    BackendSummarizer, SummarizeError, Summarizer, SummarizingCompactionProvider,
    CONTEXT_SUMMARY_PURPOSE,
};
