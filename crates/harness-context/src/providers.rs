//! Concrete [`ContextProvider`] implementations.

use std::sync::Arc;

use async_trait::async_trait;

use harness_protocol::backend::ExecutionRequest;
use harness_protocol::ids::{MessageId, Timestamp};
use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use harness_runtime::traits::Workspace;

use crate::provider::ContextProvider;

/// Prepends a fixed instruction string to the system prompt — the
/// IDE-integration equivalent of a `CLAUDE.md`: a static project/product
/// instruction set that every request should carry.
pub struct StaticSystemPromptProvider {
    instructions: String,
}

impl StaticSystemPromptProvider {
    pub fn new(instructions: impl Into<String>) -> Self {
        Self {
            instructions: instructions.into(),
        }
    }
}

#[async_trait]
impl ContextProvider for StaticSystemPromptProvider {
    async fn assemble(&self, mut request: ExecutionRequest, _workspace: &dyn Workspace) -> ExecutionRequest {
        request.system_prompt = if request.system_prompt.is_empty() {
            self.instructions.clone()
        } else {
            format!("{}\n\n{}", self.instructions, request.system_prompt)
        };
        request
    }
}

/// Appends a short workspace summary (root path + a shallow file listing) to
/// the system prompt so the agent has basic orientation without spending a
/// tool round-trip on "where am I."
pub struct WorkspaceInfoProvider {
    max_files: usize,
}

impl WorkspaceInfoProvider {
    pub fn new(max_files: usize) -> Self {
        Self { max_files }
    }
}

impl Default for WorkspaceInfoProvider {
    /// 50 entries keeps the summary compact for typical projects without
    /// needing tuning; pass an explicit value via [`Self::new`] for larger
    /// workspaces.
    fn default() -> Self {
        Self::new(50)
    }
}

#[async_trait]
impl ContextProvider for WorkspaceInfoProvider {
    async fn assemble(&self, mut request: ExecutionRequest, workspace: &dyn Workspace) -> ExecutionRequest {
        let root = workspace.root().display().to_string();
        let summary = match workspace.list_files(1).await {
            Ok(files) => {
                let overflow = files.len().saturating_sub(self.max_files);
                let mut lines: Vec<String> = files
                    .iter()
                    .take(self.max_files)
                    .map(|file| file.path.display().to_string())
                    .collect();
                if overflow > 0 {
                    lines.push(format!("... ({overflow} more)"));
                }
                if lines.is_empty() {
                    format!("Workspace root: {root}")
                } else {
                    format!("Workspace root: {root}\nTop-level entries:\n{}", lines.join("\n"))
                }
            }
            // A workspace that can't list files (e.g. a restricted or
            // not-yet-populated one) still gets a usable prompt — just
            // without the listing, rather than failing the whole request.
            Err(_) => format!("Workspace root: {root}"),
        };

        request.system_prompt = if request.system_prompt.is_empty() {
            summary
        } else {
            format!("{}\n\n{summary}", request.system_prompt)
        };
        request
    }
}

/// Wraps another provider; when the transcript's rough character count
/// exceeds `max_chars`, drops all but the most recent `keep_recent` messages
/// and replaces the dropped ones with a single synthetic system note.
///
/// This is deliberately a coarse heuristic, not exact tokenization — exact
/// token counting is provider-specific (a workspace concern this crate
/// doesn't own) and precision here matters less than simply avoiding
/// catastrophic context-window overflow on long sessions. Real LLM-generated
/// summarization of the dropped turns is a natural upgrade once this coarser
/// truncation proves the wiring works end to end.
pub struct TruncatingCompactionProvider {
    inner: Arc<dyn ContextProvider>,
    max_chars: usize,
    keep_recent: usize,
}

impl TruncatingCompactionProvider {
    pub fn new(inner: Arc<dyn ContextProvider>, max_chars: usize, keep_recent: usize) -> Self {
        Self {
            inner,
            max_chars,
            keep_recent,
        }
    }
}

fn message_char_len(message: &AgentMessage) -> usize {
    message
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::ToolResult { result, .. } => result.output_preview.len(),
            ContentBlock::ToolUse { .. } | ContentBlock::Image { .. } => 0,
        })
        .sum()
}

fn truncation_note(dropped: usize) -> AgentMessage {
    AgentMessage {
        id: MessageId::new(),
        role: MessageRole::System,
        content: vec![ContentBlock::Text {
            text: format!("[earlier conversation truncated — {dropped} message(s) omitted]"),
        }],
        created_at: Timestamp::now(),
    }
}

#[async_trait]
impl ContextProvider for TruncatingCompactionProvider {
    async fn assemble(&self, request: ExecutionRequest, workspace: &dyn Workspace) -> ExecutionRequest {
        let mut request = self.inner.assemble(request, workspace).await;

        let total_chars: usize = request.messages.iter().map(message_char_len).sum();
        if total_chars <= self.max_chars || request.messages.len() <= self.keep_recent {
            return request;
        }

        let keep_from = request.messages.len() - self.keep_recent;
        let dropped = keep_from;
        let mut compacted = Vec::with_capacity(self.keep_recent + 1);
        compacted.push(truncation_note(dropped));
        compacted.extend(request.messages.split_off(keep_from));
        request.messages = compacted;
        request
    }
}

/// Composes a sequence of providers, running each in order — e.g.
/// `WorkspaceInfoProvider` then `TruncatingCompactionProvider`.
pub struct ChainedContextProvider {
    providers: Vec<Arc<dyn ContextProvider>>,
}

impl ChainedContextProvider {
    pub fn new(providers: Vec<Arc<dyn ContextProvider>>) -> Self {
        Self { providers }
    }
}

#[async_trait]
impl ContextProvider for ChainedContextProvider {
    async fn assemble(&self, mut request: ExecutionRequest, workspace: &dyn Workspace) -> ExecutionRequest {
        for provider in &self.providers {
            request = provider.assemble(request, workspace).await;
        }
        request
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use harness_protocol::ids::{RequestId, RunId};
    use harness_runtime::workspace::FakeWorkspace;

    fn empty_request() -> ExecutionRequest {
        ExecutionRequest {
            request_id: RequestId::new(),
            run_id: RunId::new(),
            system_prompt: String::new(),
            messages: vec![],
            tools: vec![],
            extended_thinking: false,
        }
    }

    fn text_message(role: MessageRole, text: &str) -> AgentMessage {
        AgentMessage {
            id: MessageId::new(),
            role,
            content: vec![ContentBlock::Text { text: text.to_string() }],
            created_at: Timestamp::now(),
        }
    }

    #[tokio::test]
    async fn static_prompt_seeds_an_empty_system_prompt() {
        let provider = StaticSystemPromptProvider::new("be helpful");
        let workspace = FakeWorkspace::new();
        let result = provider.assemble(empty_request(), &workspace).await;
        assert_eq!(result.system_prompt, "be helpful");
    }

    #[tokio::test]
    async fn static_prompt_prepends_to_an_existing_prompt() {
        let provider = StaticSystemPromptProvider::new("be helpful");
        let workspace = FakeWorkspace::new();
        let mut request = empty_request();
        request.system_prompt = "existing".to_string();
        let result = provider.assemble(request, &workspace).await;
        assert_eq!(result.system_prompt, "be helpful\n\nexisting");
    }

    #[tokio::test]
    async fn workspace_info_includes_root_path() {
        let provider = WorkspaceInfoProvider::default();
        let workspace = FakeWorkspace::new();
        let result = provider.assemble(empty_request(), &workspace).await;
        assert!(result.system_prompt.contains("Workspace root:"));
    }

    #[tokio::test]
    async fn compaction_is_a_noop_under_the_char_budget() {
        let base = Arc::new(StaticSystemPromptProvider::new("noop"));
        let provider = TruncatingCompactionProvider::new(base, 10_000, 2);
        let workspace = FakeWorkspace::new();
        let mut request = empty_request();
        request.messages = vec![
            text_message(MessageRole::User, "hi"),
            text_message(MessageRole::Assistant, "hello"),
        ];
        let result = provider.assemble(request, &workspace).await;
        assert_eq!(result.messages.len(), 2);
    }

    #[tokio::test]
    async fn compaction_truncates_when_over_budget() {
        let base = Arc::new(StaticSystemPromptProvider::new("noop"));
        let provider = TruncatingCompactionProvider::new(base, 10, 1);
        let workspace = FakeWorkspace::new();
        let mut request = empty_request();
        request.messages = vec![
            text_message(MessageRole::User, "this message is long enough to exceed budget"),
            text_message(MessageRole::Assistant, "so is this one honestly"),
            text_message(MessageRole::User, "most recent message"),
        ];
        let result = provider.assemble(request, &workspace).await;
        // Truncation note + the single kept (most recent) message.
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].role, MessageRole::System);
        assert!(matches!(
            &result.messages[0].content[0],
            ContentBlock::Text { text } if text.contains("truncated")
        ));
        assert!(matches!(
            &result.messages[1].content[0],
            ContentBlock::Text { text } if text == "most recent message"
        ));
    }

    #[tokio::test]
    async fn chained_provider_runs_each_in_order() {
        let chain = ChainedContextProvider::new(vec![
            Arc::new(StaticSystemPromptProvider::new("first")),
            Arc::new(StaticSystemPromptProvider::new("second")),
        ]);
        let workspace = FakeWorkspace::new();
        let result = chain.assemble(empty_request(), &workspace).await;
        // Each StaticSystemPromptProvider prepends, so "second" (applied
        // last) ends up in front of "first".
        assert_eq!(result.system_prompt, "second\n\nfirst");
    }
}
