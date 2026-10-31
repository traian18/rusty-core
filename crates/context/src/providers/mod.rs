//! Concrete [`ContextProvider`](crate::provider::ContextProvider) implementations.
//!
//! Each provider is one small module: a fixed system prompt, workspace info, the three ways of
//! compacting history (by characters, by the model's token budget, with summaries elsewhere), and
//! a chain that applies several in order.

mod chained;
mod policy_driven;
mod static_prompt;
#[cfg(test)]
mod tests;
mod truncating;
mod workspace_info;

pub use chained::ChainedContextProvider;
pub(crate) use policy_driven::estimate_tokens;
pub use policy_driven::{CompactionRecord, PolicyDrivenCompactionProvider};
pub use static_prompt::StaticSystemPromptProvider;
pub use truncating::TruncatingCompactionProvider;
pub use workspace_info::WorkspaceInfoProvider;
