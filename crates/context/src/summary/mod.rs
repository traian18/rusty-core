//! Tier 3: background LLM summarization of aged-out history.
//!
//! [`SummarizingCompactionProvider`] never blocks a request on a model call.
//! Under soft pressure it starts a background task that (optionally) asks an
//! [`ImportanceJudge`] which old messages matter, then summarizes the rest,
//! folding in the previous summary so nothing is summarized twice. A finished
//! summary is applied on a later request; until one is ready, or if the
//! summarizer fails, the cheap tiers in the fallback provider keep the
//! request in budget.
//!
//! The provider (`provider`) decides when to start; a background task (`background`) writes the
//! summary with a [`Summarizer`] such as the backend one (`backend`); `state` is what the provider
//! remembers between requests.

mod backend;
mod background;
mod provider;
mod state;
#[cfg(test)]
mod tests;

use async_trait::async_trait;

use rusty_protocol::messages::AgentMessage;

pub use backend::BackendSummarizer;
pub use provider::SummarizingCompactionProvider;

#[derive(Debug, Clone)]
pub struct SummarizeError(pub String);

impl std::fmt::Display for SummarizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SummarizeError {}

/// Condenses messages into a running summary.
#[async_trait]
pub trait Summarizer: Send + Sync {
    /// `previous` is the summary so far; fold `messages` into it and return
    /// the complete new summary.
    async fn summarize(
        &self,
        previous: Option<&str>,
        messages: &[AgentMessage],
    ) -> Result<String, SummarizeError>;
}

/// `params.provider_options.rusty.purpose` of every summarizer request.
pub const CONTEXT_SUMMARY_PURPOSE: &str = "context_summary";
