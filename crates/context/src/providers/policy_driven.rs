//! Compaction driven by a model's context budget instead of a character count.

use rusty_runtime::locks::MutexExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use rusty_protocol::backend::ExecutionRequest;
use rusty_protocol::ids::MessageId;

use rusty_runtime::traits::Workspace;

use crate::compaction::{drop_prefix, elide_tool_results, truncation_note, Retention};
use crate::observability::{
    CompactionMode, CompactionPhase, CompactionTelemetry, ContextObserver, MeasurementStage,
};
use crate::policy::{ContextBudget, ContextDecision, ContextPolicy, TokenEstimate};
use crate::provider::{ContextObservation, ContextProvider};
use crate::providers::truncating::message_char_len;
use crate::visibility::outcome_of;
use crate::visibility::CompactionOperation;
use rusty_protocol::events::CompactionType;

/// A record of one compaction decision, kept for observability.
///
/// This is provider-instance-scoped bookkeeping (in-memory, not yet part of
/// the durable per-agent snapshot) — see
/// [`PolicyDrivenCompactionProvider::last_compaction`] for the caveat on
/// what "durable lineage" means today versus the fuller cross-crate wiring
/// (updating `rusty_agent_core::context_state::AgentContextState` via a real
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
    observer: Option<ContextObserver>,
    compaction_count: AtomicU64,
}

/// Chars-per-token approximation used when no real tokenizer is available.
/// Deliberately conservative (undercounts tokens... no — overcounts input
/// size relative to typical English text, which averages ~4 chars/token) so
/// this errs toward compacting a little early rather than a little late.
pub const APPROX_CHARS_PER_TOKEN: usize = 4;

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
            observer: None,
            compaction_count: AtomicU64::new(0),
        }
    }

    /// Attach bounded, non-blocking context telemetry.
    pub fn with_observer(mut self, observer: ContextObserver) -> Self {
        self.observer = Some(observer);
        self
    }

    /// The most recent compaction decision this provider instance made, if
    /// any. See the struct doc comment for what "durable" does and doesn't
    /// mean here today.
    pub fn last_compaction(&self) -> Option<CompactionRecord> {
        *self.last_compaction.lock_or_recover()
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
        *self.last_compaction.lock_or_recover() = Some(CompactionRecord {
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
        let request = self.inner.assemble(request, workspace).await;
        self.compact_observed(request, None).await
    }

    /// As `assemble`, reporting the compaction as one standard operation when it has work to do,
    /// including the unknown-window fallback.
    async fn assemble_with_observation(
        &self,
        request: ExecutionRequest,
        workspace: &dyn Workspace,
        observation: &ContextObservation,
    ) -> ExecutionRequest {
        let request = self
            .inner
            .assemble_with_observation(request, workspace, observation)
            .await;
        self.compact_observed(request, Some(observation)).await
    }
}

impl PolicyDrivenCompactionProvider {
    async fn compact_observed(
        &self,
        request: ExecutionRequest,
        observation: Option<&ContextObservation>,
    ) -> ExecutionRequest {
        let before = estimate_tokens(&request).tokens;
        let triggered = match self
            .policy
            .evaluate(self.context_window, estimate_tokens(&request))
        {
            ContextDecision::CompactBeforeRequest { .. } => true,
            ContextDecision::Unavailable { .. } if self.context_window.is_none() => {
                total_chars_of(&request) > self.fallback_max_chars
            }
            _ => false,
        };
        let telemetry = CompactionTelemetry {
            observer: self.observer.clone(),
            conversation_id: request.messages.first().map(|m| m.id),
            operation_id: MessageId::new(),
            mode: CompactionMode::Standard,
        };
        if let Some(observer) = &self.observer {
            observer.measure(
                &request,
                self.policy,
                self.context_window,
                MeasurementStage::BeforeCompaction,
            );
        }
        let operation = match (triggered, observation) {
            (true, Some(observation)) => {
                CompactionOperation::start(observation, CompactionType::Standard, Some(before))
                    .await
            }
            _ => None,
        };
        if triggered {
            telemetry.update(CompactionPhase::Started, Some(before), None);
        }
        let request = self.prepare(request);
        let after = estimate_tokens(&request).tokens;
        if triggered {
            let phase = if after < before {
                CompactionPhase::Completed
            } else {
                CompactionPhase::NoChange
            };
            telemetry.update(phase, Some(before), Some(after));
        }
        if let Some(operation) = operation {
            operation.finish(outcome_of(observation), Some(after)).await;
        }
        if let Some(observer) = &self.observer {
            observer.measure(
                &request,
                self.policy,
                self.context_window,
                MeasurementStage::AfterCompaction,
            );
        }
        request
    }
}

impl PolicyDrivenCompactionProvider {
    fn prepare(&self, request: ExecutionRequest) -> ExecutionRequest {
        let Some(context_window) = self.context_window else {
            return self.compact_without_window(request);
        };
        let projected = estimate_tokens(&request);
        match self.policy.evaluate(Some(context_window), projected) {
            // Soft-limit pressure is a signal for a host to schedule *background* compaction
            // ahead of the next request; this provider has no async background task to hand that
            // off to, so the request proceeds unmodified, as it does below the soft limit.
            ContextDecision::Proceed { .. }
            | ContextDecision::ScheduleBackgroundCompaction { .. }
            | ContextDecision::Unavailable { .. } => request,
            ContextDecision::CompactBeforeRequest { budget } => {
                self.compact_to_budget(request, projected, budget)
            }
        }
    }
}

impl PolicyDrivenCompactionProvider {
    /// Unknown window: no policy-based sizing is possible, but the payload must not be unbounded,
    /// so the flat character cap applies instead.
    fn compact_without_window(&self, mut request: ExecutionRequest) -> ExecutionRequest {
        let total_chars = total_chars_of(&request);
        if total_chars <= self.fallback_max_chars {
            return request;
        }
        let projected_tokens = total_chars / APPROX_CHARS_PER_TOKEN;
        if self.elide_old_results(&mut request)
            && total_chars_of(&request) <= self.fallback_max_chars
        {
            self.record(projected_tokens, false, 0, request.messages.len());
            return request;
        }
        if request.messages.len() <= self.keep_recent {
            return request;
        }
        let kept = self.keep_recent;
        let compacted = self.compact_to(request, kept);
        // Unknown window: there is no policy pressure figure to record.
        self.record(projected_tokens, false, 0, kept);
        compacted
    }

    /// Tier 1 elides bulky old tool output; if that alone reaches the target no turn is dropped.
    /// Otherwise the oldest messages beyond `keep_recent` go until the rest fits, as in
    /// `TruncatingCompactionProvider` but budgeted from the policy's target.
    fn compact_to_budget(
        &self,
        mut request: ExecutionRequest,
        projected: TokenEstimate,
        budget: ContextBudget,
    ) -> ExecutionRequest {
        let target_tokens = self.policy.target_tokens(budget.input_budget, false);
        let target_chars = (target_tokens as usize).saturating_mul(APPROX_CHARS_PER_TOKEN);
        if self.elide_old_results(&mut request) && total_chars_of(&request) <= target_chars {
            self.record(
                projected.tokens as usize,
                projected.exact,
                budget.pressure_percent,
                request.messages.len(),
            );
            return request;
        }
        let kept = self.messages_to_keep(&request, target_chars);
        let compacted = self.compact_to(request, kept);
        self.record(
            projected.tokens as usize,
            projected.exact,
            budget.pressure_percent,
            kept,
        );
        compacted
    }

    /// How many of the newest messages fit in `target_chars`, never fewer than one.
    fn messages_to_keep(&self, request: &ExecutionRequest, target_chars: usize) -> usize {
        let mut kept = request.messages.len().min(self.keep_recent.max(1));
        while kept > 1 {
            let tail = &request.messages[request.messages.len() - kept..];
            if tail.iter().map(message_char_len).sum::<usize>() <= target_chars {
                break;
            }
            kept -= 1;
        }
        kept
    }

    /// Whether any old tool output was elided.
    fn elide_old_results(&self, request: &mut ExecutionRequest) -> bool {
        elide_tool_results(&mut request.messages, self.keep_recent, &self.retention) > 0
    }
}

fn total_chars_of(request: &ExecutionRequest) -> usize {
    request.messages.iter().map(message_char_len).sum()
}
