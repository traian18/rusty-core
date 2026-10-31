//! [`ContextProvider`]: the async, workspace-aware counterpart of
//! `rusty-agent-core`'s pure `Agent::execution_request` (see
//! `crates/rusty-agent-core/src/transitions.rs`), which builds every
//! [`ExecutionRequest`] straight from `AgentState::system_prompt` /
//! `messages` with no injection or compaction. `Agent::apply` is documented
//! as a deterministic, synchronous state machine, so context assembly that
//! reads live workspace state (or, eventually, calls a model for
//! summarization) cannot live there — it runs one layer up, wrapping the
//! [`ExecutionBackend`](rusty_runtime::traits::ExecutionBackend) instead.

use async_trait::async_trait;
use rusty_protocol::backend::ExecutionRequest;
use rusty_runtime::traits::Workspace;

/// Rewrites an outgoing [`ExecutionRequest`]'s `system_prompt`/`messages`
/// before it reaches the backend.
///
/// Implementations receive the session's [`Workspace`] so they can pull in
/// live project state (file listings, a project instructions file) without
/// a tool round-trip.
#[async_trait]
pub trait ContextProvider: Send + Sync {
    async fn assemble(
        &self,
        request: ExecutionRequest,
        workspace: &dyn Workspace,
    ) -> ExecutionRequest;
}
