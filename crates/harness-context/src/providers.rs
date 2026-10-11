//! Concrete [`ContextProvider`] implementations.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use harness_protocol::backend::ExecutionRequest;
use harness_protocol::ids::MessageId;
use harness_protocol::messages::{AgentMessage, ContentBlock};
use harness_runtime::traits::Workspace;

use crate::compaction::{drop_prefix, elide_tool_results, truncation_note, Retention};
use crate::policy::{ContextDecision, ContextPolicy, TokenEstimate};
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
    async fn assemble(
        &self,
        mut request: ExecutionRequest,
        _workspace: &dyn Workspace,
    ) -> ExecutionRequest {
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
    async fn assemble(
        &self,
        mut request: ExecutionRequest,
        workspace: &dyn Workspace,
    ) -> ExecutionRequest {
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
                    format!(
                        "Workspace root: {root}\nTop-level entries:\n{}",
                        lines.join("\n")
                    )
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
    retention: Retention,
}

impl TruncatingCompactionProvider {
    pub fn new(inner: Arc<dyn ContextProvider>, max_chars: usize, keep_recent: usize) -> Self {
        Self {
            inner,
            max_chars,
            keep_recent,
            retention: Retention::default(),
        }
    }

    /// Messages that must survive compaction regardless of age.
    pub fn with_pinned_messages(mut self, ids: impl IntoIterator<Item = MessageId>) -> Self {
        self.retention.pinned.extend(ids);
        self
    }

    /// Also keep the first user message (the original task) verbatim.
    pub fn with_anchor_first_user_message(mut self, anchor: bool) -> Self {
        self.retention.anchor_first_user = anchor;
        self
    }
}

/// Rough size of one image in the character budget (~1.5k tokens).
const IMAGE_CHARS: usize = 6_000;

fn message_char_len(message: &AgentMessage) -> usize {
    message
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::ToolResult { result, .. } => result.output_preview.len(),
            // Arguments can be large (a whole file for a write), and the
            // model is sent them on every later turn.
            ContentBlock::ToolUse { call } => call.name.len() + call.arguments.to_string().len(),
            ContentBlock::Image { .. } => IMAGE_CHARS,
        })
        .sum()
}

#[async_trait]
impl ContextProvider for TruncatingCompactionProvider {
    async fn assemble(
        &self,
        request: ExecutionRequest,
        workspace: &dyn Workspace,
    ) -> ExecutionRequest {
        let mut request = self.inner.assemble(request, workspace).await;

        let total_chars: usize = request.messages.iter().map(message_char_len).sum();
        if total_chars <= self.max_chars {
            return request;
        }

        // Tier 1: shrink old tool output before discarding any turn.
        elide_tool_results(&mut request.messages, self.keep_recent, &self.retention);
        let total_chars: usize = request.messages.iter().map(message_char_len).sum();
        if total_chars <= self.max_chars {
            return request;
        }

        // Tier 2: drop the oldest turns at a tool-pair-safe cut.
        drop_prefix(
            &mut request.messages,
            self.keep_recent,
            &self.retention,
            truncation_note,
        );
        request
    }
}

/// A record of one compaction decision, kept for observability.
///
/// This is provider-instance-scoped bookkeeping (in-memory, not yet part of
/// the durable per-agent snapshot) — see
/// [`PolicyDrivenCompactionProvider::last_compaction`] for the caveat on
/// what "durable lineage" means today versus the fuller cross-crate wiring
/// (updating `harness_core::context_state::AgentContextState` via a real
/// command round-trip) that would be needed to make this survive a restart.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompactionRecord {
    /// Estimated input tokens *before* compaction.
    pub projected_input_tokens: u64,
    /// Whether the estimate came from a real tokenizer (always `false`
    /// today — see the char/4 approximation note on
    /// [`PolicyDrivenCompactionProvider`]).
    pub exact: bool,
    /// Pressure percentage that triggered this decision (100 = exactly at
    /// the safe input budget).
    pub pressure_percent: u16,
    /// How many trailing messages were kept after compaction.
    pub kept_messages: usize,
}

/// Token-budget-aware compaction, driven by [`ContextPolicy`] rather than a
/// fixed byte count.
///
/// Estimates the projected input size with a conservative
/// characters-divided-by-4 heuristic (`TokenEstimate::approximate` — real
/// per-provider tokenization is out of scope here, matching this crate's
/// existing stance that exact token counting is provider-specific), asks
/// `ContextPolicy::evaluate` whether that's proceed / schedule-background /
/// compact-now pressure against the model's real context window, and only
/// then drops trailing messages down to the policy's `target_tokens` — as
/// opposed to [`TruncatingCompactionProvider`]'s fixed `max_chars` cutoff,
/// which has no notion of *which* model it's assembling for.
///
/// `context_window` is supplied at construction (from the session's chosen
/// model's `ModelDescriptor::context_window`) rather than looked up
/// per-request — reasonable since a session's model rarely changes
/// mid-conversation; a session that changes model mid-flight (M4's
/// `ConfigureExecution`) will keep compacting against the window it was
/// built with until the provider is reconstructed. When `context_window` is
/// `None` (unknown model, or a model whose catalog entry doesn't report
/// one), this falls back to `TruncatingCompactionProvider`'s behavior rather
/// than silently skipping compaction — a known model gets policy-aware
/// sizing, an unknown one still gets a safety-net cap.
pub struct PolicyDrivenCompactionProvider {
    inner: Arc<dyn ContextProvider>,
    policy: ContextPolicy,
    context_window: Option<u64>,
    keep_recent: usize,
    /// Fallback raw character cap used only when `context_window` is `None`.
    fallback_max_chars: usize,
    retention: Retention,
    last_compaction: Mutex<Option<CompactionRecord>>,
    compaction_count: AtomicU64,
}

/// Chars-per-token approximation used when no real tokenizer is available.
/// Deliberately conservative (undercounts tokens... no — overcounts input
/// size relative to typical English text, which averages ~4 chars/token) so
/// this errs toward compacting a little early rather than a little late.
const APPROX_CHARS_PER_TOKEN: usize = 4;

pub(crate) fn estimate_tokens(request: &ExecutionRequest) -> TokenEstimate {
    let system_chars = request.system_prompt.len();
    let message_chars: usize = request.messages.iter().map(message_char_len).sum();
    let total_chars = system_chars.saturating_add(message_chars);
    TokenEstimate::approximate((total_chars / APPROX_CHARS_PER_TOKEN) as u64)
}

impl PolicyDrivenCompactionProvider {
    pub fn new(
        inner: Arc<dyn ContextProvider>,
        policy: ContextPolicy,
        context_window: Option<u64>,
        keep_recent: usize,
        fallback_max_chars: usize,
    ) -> Self {
        Self {
            inner,
            policy,
            context_window,
            keep_recent,
            fallback_max_chars,
            retention: Retention::default(),
            last_compaction: Mutex::new(None),
            compaction_count: AtomicU64::new(0),
        }
    }

    /// The most recent compaction decision this provider instance made, if
    /// any. See the struct doc comment for what "durable" does and doesn't
    /// mean here today.
    pub fn last_compaction(&self) -> Option<CompactionRecord> {
        *self
            .last_compaction
            .lock()
            .expect("last_compaction mutex poisoned")
    }

    /// Total number of times this provider instance has actually compacted
    /// (not counted: `Proceed`/`ScheduleBackgroundCompaction` decisions that
    /// didn't truncate anything).
    pub fn compaction_count(&self) -> u64 {
        self.compaction_count.load(Ordering::Relaxed)
    }

    /// Messages that must survive compaction regardless of age.
    pub fn with_pinned_messages(mut self, ids: impl IntoIterator<Item = MessageId>) -> Self {
        self.retention.pinned.extend(ids);
        self
    }

    /// Also keep the first user message (the original task) verbatim.
    pub fn with_anchor_first_user_message(mut self, anchor: bool) -> Self {
        self.retention.anchor_first_user = anchor;
        self
    }

    fn record(&self, tokens: usize, exact: bool, pressure_percent: u16, kept_messages: usize) {
        *self
            .last_compaction
            .lock()
            .expect("last_compaction mutex poisoned") = Some(CompactionRecord {
            projected_input_tokens: tokens as u64,
            exact,
            pressure_percent,
            kept_messages,
        });
        self.compaction_count.fetch_add(1, Ordering::Relaxed);
    }

    fn compact_to(&self, mut request: ExecutionRequest, keep: usize) -> ExecutionRequest {
        drop_prefix(
            &mut request.messages,
            keep,
            &self.retention,
            truncation_note,
        );
        request
    }
}

#[async_trait]
impl ContextProvider for PolicyDrivenCompactionProvider {
    async fn assemble(
        &self,
        request: ExecutionRequest,
        workspace: &dyn Workspace,
    ) -> ExecutionRequest {
        let mut request = self.inner.assemble(request, workspace).await;

        let Some(context_window) = self.context_window else {
            // Unknown window: no policy-based sizing is possible, but we
            // still don't want a genuinely unbounded payload — fall back to
            // the flat character cap rather than skip compaction entirely.
            let total_chars: usize = request.messages.iter().map(message_char_len).sum();
            if total_chars <= self.fallback_max_chars {
                return request;
            }
            if elide_tool_results(&mut request.messages, self.keep_recent, &self.retention) > 0 {
                let after: usize = request.messages.iter().map(message_char_len).sum();
                if after <= self.fallback_max_chars {
                    self.record(
                        total_chars / APPROX_CHARS_PER_TOKEN,
                        false,
                        0,
                        request.messages.len(),
                    );
                    return request;
                }
            }
            if request.messages.len() <= self.keep_recent {
                return request;
            }
            let kept = self.keep_recent;
            let compacted = self.compact_to(request, kept);
            *self
                .last_compaction
                .lock()
                .expect("last_compaction mutex poisoned") = Some(CompactionRecord {
                projected_input_tokens: (total_chars / APPROX_CHARS_PER_TOKEN) as u64,
                exact: false,
                pressure_percent: 0, // unknown window: no policy pressure figure available
                kept_messages: kept,
            });
            self.compaction_count.fetch_add(1, Ordering::Relaxed);
            return compacted;
        };

        let projected_input = estimate_tokens(&request);
        let decision = self.policy.evaluate(Some(context_window), projected_input);

        match decision {
            ContextDecision::Proceed { .. }
            | ContextDecision::ScheduleBackgroundCompaction { .. } => {
                // Soft-limit pressure is a signal for a host to schedule
                // *background* compaction ahead of the next request; this
                // provider has no async background task to hand that off to,
                // so — same as the hard-limit case not yet being reached —
                // the request proceeds unmodified. See the module docs.
                request
            }
            ContextDecision::CompactBeforeRequest { budget } => {
                let target_tokens = self.policy.target_tokens(budget.input_budget, false);
                let target_chars = (target_tokens as usize).saturating_mul(APPROX_CHARS_PER_TOKEN);
                // Tier 1: elide bulky old tool output; if that alone reaches
                // the target, no turn needs to be dropped.
                if elide_tool_results(&mut request.messages, self.keep_recent, &self.retention) > 0
                {
                    let after: usize = request.messages.iter().map(message_char_len).sum();
                    if after <= target_chars {
                        self.record(
                            projected_input.tokens as usize,
                            projected_input.exact,
                            budget.pressure_percent,
                            request.messages.len(),
                        );
                        return request;
                    }
                }
                // Keep dropping trailing-but-oldest messages (beyond
                // `keep_recent`) until under the target, same mechanics as
                // `TruncatingCompactionProvider`, just budgeted from the
                // policy's target instead of a fixed constant.
                let mut kept = request.messages.len().min(self.keep_recent.max(1));
                loop {
                    let remaining_chars: usize = request.messages[request.messages.len() - kept..]
                        .iter()
                        .map(message_char_len)
                        .sum();
                    if remaining_chars <= target_chars || kept <= 1 {
                        break;
                    }
                    kept -= 1;
                }
                let compacted = self.compact_to(request, kept);
                *self
                    .last_compaction
                    .lock()
                    .expect("last_compaction mutex poisoned") = Some(CompactionRecord {
                    projected_input_tokens: projected_input.tokens,
                    exact: projected_input.exact,
                    pressure_percent: budget.pressure_percent,
                    kept_messages: kept,
                });
                self.compaction_count.fetch_add(1, Ordering::Relaxed);
                compacted
            }
            ContextDecision::Unavailable { .. } => request,
        }
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
    async fn assemble(
        &self,
        mut request: ExecutionRequest,
        workspace: &dyn Workspace,
    ) -> ExecutionRequest {
        for provider in &self.providers {
            request = provider.assemble(request, workspace).await;
        }
        request
    }
}

#[cfg(test)]
mod tests;
