//! Tier 3: background LLM summarization of aged-out history.
//!
//! [`SummarizingCompactionProvider`] never blocks a request on a model call.
//! Under soft pressure it starts a background task that (optionally) asks an
//! [`ImportanceJudge`] which old messages matter, then summarizes the rest,
//! folding in the previous summary so nothing is summarized twice. A finished
//! summary is applied on a later request; until one is ready, or if the
//! summarizer fails, the cheap tiers in the fallback provider keep the
//! request in budget.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use harness_protocol::backend::{ExecutionEvent, ExecutionParams, ExecutionRequest};
use harness_protocol::ids::{MessageId, RequestId, RunId, Timestamp};
use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use harness_runtime::traits::{ExecutionBackend, Workspace};

use crate::compaction::{drop_prefix, truncation_note, Retention, SUMMARY_MARKER};
use crate::importance::{clip, Importance, ImportanceJudge, JudgeItem};
use crate::policy::{ContextDecision, ContextPolicy};
use crate::provider::ContextProvider;
use crate::providers::estimate_tokens;

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

const SUMMARY_SYSTEM_PROMPT: &str = "You compress the earlier part of a coding assistant's conversation so work can continue from the summary alone. Merge the previous summary (if any) with the new messages into one updated summary. Preserve: the user's goals and constraints, decisions made and why, files created or changed, errors hit and how they were resolved, and open questions. Drop pleasantries and raw tool output; keep exact names, paths and numbers. Reply with the summary only, as terse bullet points.";

/// Summarizes through any [`ExecutionBackend`], so a small cheap model can be
/// pinned with `model`.
pub struct BackendSummarizer {
    backend: Arc<dyn ExecutionBackend>,
    model: Option<String>,
    max_output_tokens: u64,
    max_input_chars: usize,
}

impl BackendSummarizer {
    pub fn new(backend: Arc<dyn ExecutionBackend>) -> Self {
        Self {
            backend,
            model: None,
            max_output_tokens: 1_500,
            max_input_chars: 60_000,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_max_output_tokens(mut self, tokens: u64) -> Self {
        self.max_output_tokens = tokens;
        self
    }
}

/// Plain-text transcript for the summarizer, each message clipped.
fn transcript(messages: &[AgentMessage], max_chars: usize) -> String {
    let per_message = (max_chars / messages.len().max(1)).clamp(200, 4_000);
    let mut out = String::new();
    for message in messages {
        let mut body = String::new();
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => body.push_str(text),
                ContentBlock::ToolUse { call } => {
                    body.push_str(&format!("[called {} {}]", call.name, call.arguments))
                }
                ContentBlock::ToolResult { result, .. } => body.push_str(&format!(
                    "[tool result{}] {}",
                    if result.has_error { " (error)" } else { "" },
                    result.output_preview
                )),
                ContentBlock::Image { .. } => body.push_str("[image]"),
            }
            body.push('\n');
        }
        out.push_str(&format!(
            "[{:?}] {}\n\n",
            message.role,
            clip(body.trim(), per_message)
        ));
    }
    out
}

#[async_trait]
impl Summarizer for BackendSummarizer {
    async fn summarize(
        &self,
        previous: Option<&str>,
        messages: &[AgentMessage],
    ) -> Result<String, SummarizeError> {
        let prompt = format!(
            "Previous summary:\n{}\n\nNew messages to fold in:\n{}",
            previous.unwrap_or("(none)"),
            transcript(messages, self.max_input_chars)
        );
        let request = ExecutionRequest {
            request_id: RequestId::new(),
            run_id: RunId::new(),
            system_prompt: SUMMARY_SYSTEM_PROMPT.to_string(),
            messages: vec![AgentMessage {
                id: MessageId::new(),
                role: MessageRole::User,
                content: vec![ContentBlock::Text { text: prompt }],
                created_at: Timestamp::now(),
            }],
            tools: vec![],
            extended_thinking: false,
            params: ExecutionParams {
                model: self.model.clone(),
                max_tokens: Some(self.max_output_tokens),
                // Lets hosts tell this apart from the agent's own turns (for
                // example to skip per-step model selection and to account
                // its usage separately).
                provider_options: serde_json::json!({ "rusty": { "purpose": CONTEXT_SUMMARY_PURPOSE } }),
                ..Default::default()
            },
        };

        let (tx, mut rx) = broadcast::channel(256);
        let collector = tokio::spawn(async move {
            let mut text = String::new();
            loop {
                match rx.recv().await {
                    Ok(ExecutionEvent::TextDelta { delta, .. }) => text.push_str(&delta),
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            text
        });
        let result = self
            .backend
            .execute(request, tx, CancellationToken::new())
            .await;
        let text = collector.await.unwrap_or_default();
        result.map_err(|e| SummarizeError(format!("summarizer backend failed: {e:?}")))?;
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(SummarizeError("summarizer returned no text".into()));
        }
        Ok(text)
    }
}

#[derive(Default)]
struct SummaryState {
    summary: Option<String>,
    /// Last message the summary (and verdicts) account for.
    covered_through: Option<MessageId>,
    essential: HashSet<MessageId>,
    disposable: HashSet<MessageId>,
    judged: HashSet<MessageId>,
    in_flight: bool,
}

/// Summary state per conversation, keyed by the conversation's first message.
/// Sub-agents and workflow steps inherit the session's backend, so one
/// provider sees several conversations and must never mix their summaries.
type States = Arc<Mutex<HashMap<MessageId, SummaryState>>>;

/// Clears the conversation's in-flight flag when the background task ends,
/// however it ends.
struct Flight {
    states: States,
    key: MessageId,
}

impl Drop for Flight {
    fn drop(&mut self) {
        if let Ok(mut states) = self.states.lock() {
            if let Some(state) = states.get_mut(&self.key) {
                state.in_flight = false;
            }
        }
    }
}

/// What a background pass will summarize, taken from the request before any
/// summary is applied so message ids still line up with the history.
struct Plan {
    candidates: Vec<AgentMessage>,
    boundary: MessageId,
    previous: Option<String>,
    task_text: String,
}

/// Applies finished background summaries and schedules new ones.
///
/// `fallback` runs last on every request and guards the hard limit with the
/// cheap tiers; build it as a [`PolicyDrivenCompactionProvider`] over an empty
/// [`ChainedContextProvider`] so context assembly is not run twice.
///
/// [`PolicyDrivenCompactionProvider`]: crate::providers::PolicyDrivenCompactionProvider
/// [`ChainedContextProvider`]: crate::providers::ChainedContextProvider
pub struct SummarizingCompactionProvider {
    inner: Arc<dyn ContextProvider>,
    fallback: Arc<dyn ContextProvider>,
    summarizer: Arc<dyn Summarizer>,
    judge: Option<Arc<dyn ImportanceJudge>>,
    policy: ContextPolicy,
    context_window: Option<u64>,
    keep_recent: usize,
    retention: Retention,
    states: States,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    summaries_applied: AtomicU64,
}

/// Fewer new messages than this are not worth a model call yet.
const MIN_CANDIDATES: usize = 2;
const JUDGE_EXCERPT_CHARS: usize = 600;

impl SummarizingCompactionProvider {
    pub fn new(
        inner: Arc<dyn ContextProvider>,
        fallback: Arc<dyn ContextProvider>,
        summarizer: Arc<dyn Summarizer>,
        policy: ContextPolicy,
        context_window: Option<u64>,
        keep_recent: usize,
    ) -> Self {
        Self {
            inner,
            fallback,
            summarizer,
            judge: None,
            policy,
            context_window,
            keep_recent: keep_recent.max(1),
            retention: Retention::default(),
            states: Arc::default(),
            tasks: Mutex::new(Vec::new()),
            summaries_applied: AtomicU64::new(0),
        }
    }

    /// Activate an importance judge (e.g. [`JevImportanceJudge`]): essential
    /// messages stay verbatim and disposable ones are dropped unsummarized.
    ///
    /// [`JevImportanceJudge`]: crate::importance::JevImportanceJudge
    pub fn with_importance_judge(mut self, judge: Arc<dyn ImportanceJudge>) -> Self {
        self.judge = Some(judge);
        self
    }

    pub fn with_pinned_messages(mut self, ids: impl IntoIterator<Item = MessageId>) -> Self {
        self.retention.pinned.extend(ids);
        self
    }

    pub fn with_anchor_first_user_message(mut self, anchor: bool) -> Self {
        self.retention.anchor_first_user = anchor;
        self
    }

    /// The summary held for the conversation that starts with `first_message`.
    pub fn summary(&self, first_message: MessageId) -> Option<String> {
        self.states
            .lock()
            .expect("summary state poisoned")
            .get(&first_message)
            .and_then(|state| state.summary.clone())
    }

    /// How many requests have had a summary applied.
    pub fn summaries_applied(&self) -> u64 {
        self.summaries_applied.load(Ordering::Relaxed)
    }

    /// Waits for every running background summarization (tests, shutdown).
    pub async fn wait_for_background(&self) {
        let handles = std::mem::take(&mut *self.tasks.lock().expect("tasks poisoned"));
        for handle in handles {
            let _ = handle.await;
        }
    }

    fn retention_with_verdicts(&self, state: Option<&SummaryState>) -> Retention {
        let mut retention = self.retention.clone();
        if let Some(state) = state {
            retention.pinned.extend(state.essential.iter().copied());
        }
        retention
    }

    fn pressured(&self, request: &ExecutionRequest) -> bool {
        let Some(window) = self.context_window else {
            return false;
        };
        !matches!(
            self.policy.evaluate(Some(window), estimate_tokens(request)),
            ContextDecision::Proceed { .. } | ContextDecision::Unavailable { .. }
        )
    }

    fn apply_ready_summary(&self, key: MessageId, request: &mut ExecutionRequest) {
        let (retention, summary, covered) = {
            let states = self.states.lock().expect("summary state poisoned");
            let Some(state) = states.get(&key) else {
                return;
            };
            let Some(covered) = state.covered_through else {
                return;
            };
            (
                self.retention_with_verdicts(Some(state)),
                state.summary.clone(),
                covered,
            )
        };
        // History that no longer contains the covered message was rewritten;
        // the summary no longer describes it.
        let Some(position) = request.messages.iter().position(|m| m.id == covered) else {
            return;
        };
        let keep = request.messages.len() - (position + 1);
        let dropped = drop_prefix(
            &mut request.messages,
            keep,
            &retention,
            |n| match &summary {
                Some(text) => summary_message(text, n),
                None => truncation_note(n),
            },
        );
        if dropped > 0 {
            self.summaries_applied.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The next batch to summarize, or `None` when a pass is running or there
    /// is too little new history to be worth a model call.
    fn plan(&self, key: MessageId, request: &ExecutionRequest) -> Option<Plan> {
        let states = self.states.lock().expect("summary state poisoned");
        let state = states.get(&key);
        if state.is_some_and(|s| s.in_flight) {
            return None;
        }
        let retention = self.retention_with_verdicts(state);
        let start = state
            .and_then(|s| s.covered_through)
            .and_then(|id| request.messages.iter().position(|m| m.id == id))
            .map_or(0, |p| p + 1);
        let end = request.messages.len().saturating_sub(self.keep_recent);
        if end <= start {
            return None;
        }
        // Protected messages stay verbatim, so they are not summarized, but
        // the covered boundary still advances past them.
        let candidates: Vec<AgentMessage> = (start..end)
            .filter(|&i| !retention.is_protected(&request.messages, i))
            .map(|i| request.messages[i].clone())
            .collect();
        if candidates.len() < MIN_CANDIDATES {
            return None;
        }
        Some(Plan {
            candidates,
            boundary: request.messages[end - 1].id,
            previous: state.and_then(|s| s.summary.clone()),
            task_text: task_description(&request.messages),
        })
    }

    fn spawn(&self, key: MessageId, plan: Plan) {
        {
            let mut states = self.states.lock().expect("summary state poisoned");
            let state = states.entry(key).or_default();
            if state.in_flight {
                return;
            }
            state.in_flight = true;
        }
        let flight = Flight {
            states: self.states.clone(),
            key,
        };
        let states = self.states.clone();
        let summarizer = self.summarizer.clone();
        let judge = self.judge.clone();
        let handle = tokio::spawn(async move {
            let _flight = flight;
            summarize_in_background(states, key, summarizer, judge, plan).await;
        });
        let mut tasks = self.tasks.lock().expect("tasks poisoned");
        tasks.retain(|task| !task.is_finished());
        tasks.push(handle);
    }
}

async fn summarize_in_background(
    states: States,
    key: MessageId,
    summarizer: Arc<dyn Summarizer>,
    judge: Option<Arc<dyn ImportanceJudge>>,
    plan: Plan,
) {
    let with_state = |f: &mut dyn FnMut(&mut SummaryState)| {
        let mut states = states.lock().expect("summary state poisoned");
        f(states.entry(key).or_default());
    };

    if let Some(judge) = judge {
        let mut items = Vec::new();
        with_state(&mut |state| {
            items = plan
                .candidates
                .iter()
                .filter(|m| !state.judged.contains(&m.id))
                .map(|m| JudgeItem::from_message(m, JUDGE_EXCERPT_CHARS))
                .collect();
        });
        // A failed judge only means everything is treated as Useful.
        if let Ok(verdicts) = judge.judge(&plan.task_text, &items).await {
            with_state(&mut |state| {
                for verdict in &verdicts {
                    state.judged.insert(verdict.id);
                    match verdict.importance {
                        Importance::Essential => {
                            state.essential.insert(verdict.id);
                        }
                        Importance::Disposable => {
                            state.disposable.insert(verdict.id);
                        }
                        Importance::Useful => {}
                    }
                }
            });
        }
    }

    let mut to_summarize = Vec::new();
    with_state(&mut |state| {
        to_summarize = plan
            .candidates
            .iter()
            .filter(|m| !state.essential.contains(&m.id) && !state.disposable.contains(&m.id))
            .cloned()
            .collect();
    });

    let summary = if to_summarize.is_empty() {
        plan.previous
    } else {
        match summarizer
            .summarize(plan.previous.as_deref(), &to_summarize)
            .await
        {
            Ok(text) => Some(text),
            // Leave the state untouched: the cheap tiers cover this request
            // and the next soft-pressure request retries.
            Err(_) => return,
        }
    };
    with_state(&mut |state| {
        state.summary = summary.clone();
        state.covered_through = Some(plan.boundary);
    });
}

fn summary_message(summary: &str, condensed: usize) -> AgentMessage {
    AgentMessage {
        id: MessageId::new(),
        role: MessageRole::System,
        content: vec![ContentBlock::Text {
            text: format!("{SUMMARY_MARKER} — {condensed} message(s) condensed]\n{summary}"),
        }],
        created_at: Timestamp::now(),
    }
}

/// The original request plus the latest one: what verdicts are relative to.
fn task_description(messages: &[AgentMessage]) -> String {
    let text_of = |m: &AgentMessage| {
        m.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut users = messages.iter().filter(|m| m.role == MessageRole::User);
    let first = users.next().map(text_of).unwrap_or_default();
    let last = users.next_back().map(text_of);
    match last {
        Some(last) if !last.is_empty() => format!(
            "Original request:\n{}\n\nLatest request:\n{}",
            clip(&first, 2_000),
            clip(&last, 1_000)
        ),
        _ => format!("Request:\n{}", clip(&first, 2_000)),
    }
}

#[async_trait]
impl ContextProvider for SummarizingCompactionProvider {
    async fn assemble(
        &self,
        request: ExecutionRequest,
        workspace: &dyn Workspace,
    ) -> ExecutionRequest {
        let mut request = self.inner.assemble(request, workspace).await;
        let Some(key) = request.messages.first().map(|m| m.id) else {
            return self.fallback.assemble(request, workspace).await;
        };
        // Plan on the untouched history (applying a summary removes the
        // boundary message), but only schedule if the request is still under
        // pressure once the ready summary is in.
        let plan = if self.pressured(&request) {
            self.plan(key, &request)
        } else {
            None
        };
        self.apply_ready_summary(key, &mut request);
        if let Some(plan) = plan {
            if self.pressured(&request) {
                self.spawn(key, plan);
            }
        }
        self.fallback.assemble(request, workspace).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::importance::{JudgeError, Verdict};
    use crate::providers::{
        ChainedContextProvider, PolicyDrivenCompactionProvider, StaticSystemPromptProvider,
    };
    use harness_runtime::workspace::FakeWorkspace;

    struct FakeSummarizer {
        seen: Mutex<Vec<(Option<String>, usize)>>,
        fail: bool,
    }

    #[async_trait]
    impl Summarizer for FakeSummarizer {
        async fn summarize(
            &self,
            previous: Option<&str>,
            messages: &[AgentMessage],
        ) -> Result<String, SummarizeError> {
            self.seen
                .lock()
                .unwrap()
                .push((previous.map(str::to_string), messages.len()));
            if self.fail {
                return Err(SummarizeError("boom".into()));
            }
            Ok("SUMMARY".into())
        }
    }

    /// Marks the message at `essential` index essential and `disposable` disposable.
    struct FakeJudge {
        essential: MessageId,
        disposable: MessageId,
    }

    #[async_trait]
    impl ImportanceJudge for FakeJudge {
        async fn judge(&self, _: &str, items: &[JudgeItem]) -> Result<Vec<Verdict>, JudgeError> {
            Ok(items
                .iter()
                .map(|item| Verdict {
                    id: item.id,
                    importance: if item.id == self.essential {
                        Importance::Essential
                    } else if item.id == self.disposable {
                        Importance::Disposable
                    } else {
                        Importance::Useful
                    },
                    confidence: 0.9,
                })
                .collect())
        }
    }

    fn message(role: MessageRole, tokens: usize) -> AgentMessage {
        AgentMessage {
            id: MessageId::new(),
            role,
            content: vec![ContentBlock::Text {
                text: "word ".repeat(tokens),
            }],
            created_at: Timestamp::now(),
        }
    }

    fn request(messages: Vec<AgentMessage>) -> ExecutionRequest {
        ExecutionRequest {
            request_id: RequestId::new(),
            run_id: RunId::new(),
            system_prompt: String::new(),
            messages,
            tools: vec![],
            extended_thinking: false,
            params: Default::default(),
        }
    }

    fn provider(summarizer: Arc<FakeSummarizer>, window: u64) -> SummarizingCompactionProvider {
        let noop: Arc<dyn ContextProvider> = Arc::new(ChainedContextProvider::new(vec![]));
        let fallback = Arc::new(PolicyDrivenCompactionProvider::new(
            noop.clone(),
            ContextPolicy::default(),
            Some(window),
            1,
            10_000,
        ));
        SummarizingCompactionProvider::new(
            Arc::new(StaticSystemPromptProvider::new("")),
            fallback,
            summarizer,
            ContextPolicy::default(),
            Some(window),
            1,
        )
    }

    fn big_history() -> Vec<AgentMessage> {
        // ~45k tokens against a ~36.8k input budget: past the hard limit.
        (0..6)
            .map(|i| {
                message(
                    if i % 2 == 0 {
                        MessageRole::User
                    } else {
                        MessageRole::Assistant
                    },
                    7_500,
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn summary_is_built_in_background_and_applied_next_request() {
        let summarizer = Arc::new(FakeSummarizer {
            seen: Mutex::default(),
            fail: false,
        });
        let provider = provider(summarizer.clone(), 50_000);
        let ws = FakeWorkspace::new();
        let history = big_history();

        // First request: nothing ready, the cheap tiers cover it.
        let first = provider.assemble(request(history.clone()), &ws).await;
        assert!(first.messages.len() < history.len());
        provider.wait_for_background().await;
        assert_eq!(provider.summary(history[0].id).as_deref(), Some("SUMMARY"));
        assert_eq!(
            summarizer.seen.lock().unwrap()[0].1,
            5,
            "all but keep_recent"
        );

        // Second request: the summary replaces the covered prefix.
        let second = provider.assemble(request(history.clone()), &ws).await;
        assert_eq!(second.messages.len(), 2);
        assert!(matches!(
            &second.messages[0].content[0],
            ContentBlock::Text { text } if text.contains("SUMMARY")
        ));
        assert_eq!(second.messages[1].id, history[5].id);
        assert_eq!(provider.summaries_applied(), 1);
    }

    #[tokio::test]
    async fn summarizer_failure_leaves_the_cheap_tiers_in_charge() {
        let summarizer = Arc::new(FakeSummarizer {
            seen: Mutex::default(),
            fail: true,
        });
        let provider = provider(summarizer, 50_000);
        let ws = FakeWorkspace::new();
        let history = big_history();
        provider.assemble(request(history.clone()), &ws).await;
        provider.wait_for_background().await;
        assert!(provider.summary(history[0].id).is_none());
        let again = provider.assemble(request(history.clone()), &ws).await;
        assert!(
            again.messages.len() < history.len(),
            "fallback still compacts"
        );
        assert_eq!(provider.summaries_applied(), 0);
    }

    #[tokio::test]
    async fn judge_keeps_essential_and_skips_disposable() {
        let summarizer = Arc::new(FakeSummarizer {
            seen: Mutex::default(),
            fail: false,
        });
        let history = big_history();
        let judge = Arc::new(FakeJudge {
            essential: history[1].id,
            disposable: history[2].id,
        });
        let provider = provider(summarizer.clone(), 50_000).with_importance_judge(judge);
        let ws = FakeWorkspace::new();

        provider.assemble(request(history.clone()), &ws).await;
        provider.wait_for_background().await;
        // 5 candidates: one essential, one disposable -> 3 summarized.
        assert_eq!(summarizer.seen.lock().unwrap()[0].1, 3);

        let out = provider.assemble(request(history.clone()), &ws).await;
        let ids: Vec<_> = out.messages.iter().map(|m| m.id).collect();
        assert!(ids.contains(&history[1].id), "essential kept verbatim");
        assert!(!ids.contains(&history[2].id), "disposable dropped");
        assert!(ids.contains(&history[5].id));
    }

    #[tokio::test]
    async fn below_soft_limit_never_calls_the_summarizer() {
        let summarizer = Arc::new(FakeSummarizer {
            seen: Mutex::default(),
            fail: false,
        });
        let provider = provider(summarizer.clone(), 1_000_000);
        let ws = FakeWorkspace::new();
        let out = provider.assemble(request(big_history()), &ws).await;
        provider.wait_for_background().await;
        assert_eq!(out.messages.len(), 6);
        assert!(summarizer.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn conversations_sharing_a_provider_keep_separate_summaries() {
        let summarizer = Arc::new(FakeSummarizer {
            seen: Mutex::default(),
            fail: false,
        });
        let provider = provider(summarizer.clone(), 50_000);
        let ws = FakeWorkspace::new();
        let parent = big_history();
        let child = big_history();

        provider.assemble(request(parent.clone()), &ws).await;
        provider.wait_for_background().await;
        provider.assemble(request(child.clone()), &ws).await;
        provider.wait_for_background().await;

        let seen = summarizer.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[1].0, None,
            "the child must not inherit the parent's summary"
        );
        assert!(provider.summary(parent[0].id).is_some());
        assert!(provider.summary(child[0].id).is_some());
    }

    #[tokio::test]
    async fn later_passes_continue_after_the_covered_boundary() {
        let summarizer = Arc::new(FakeSummarizer {
            seen: Mutex::default(),
            fail: false,
        });
        let provider = provider(summarizer.clone(), 50_000);
        let ws = FakeWorkspace::new();
        let mut history = big_history();

        provider.assemble(request(history.clone()), &ws).await;
        provider.wait_for_background().await;

        // Three more turns: the summary covers 0..=4, so only 5..=7 are new
        // (the last one stays as keep_recent).
        for i in 0..3 {
            history.push(message(
                if i % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                7_500,
            ));
        }
        let out = provider.assemble(request(history.clone()), &ws).await;
        provider.wait_for_background().await;

        assert!(matches!(
            &out.messages[0].content[0],
            ContentBlock::Text { text } if text.contains("SUMMARY")
        ));
        let seen = summarizer.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1], (Some("SUMMARY".to_string()), 3));
    }
}
