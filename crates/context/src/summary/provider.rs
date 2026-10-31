//! The compaction provider that folds aged-out history into a model-written summary.

use super::Summarizer;
use rusty_runtime::locks::MutexExt;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use tokio::task::JoinHandle;

use rusty_protocol::backend::ExecutionRequest;
use rusty_protocol::ids::MessageId;
use rusty_protocol::messages::AgentMessage;
use rusty_runtime::traits::Workspace;

use crate::compaction::{drop_prefix, truncation_note, Retention};
use crate::importance::ImportanceJudge;
use crate::policy::{ContextDecision, ContextPolicy};
use crate::provider::ContextProvider;
use crate::providers::estimate_tokens;

use crate::summary::background::{summarize_in_background, summary_message, task_description};
use crate::summary::state::{Flight, Plan, States, SummaryState};

/// Applies finished background summaries and schedules new ones.
///
/// `fallback` runs last on every request and guards the hard limit with the
/// cheap tiers; build it as a [`PolicyDrivenCompactionProvider`] over an empty
/// [`ChainedContextProvider`] so context assembly is not run twice.
///
/// [`PolicyDrivenCompactionProvider`]: crate::providers::PolicyDrivenCompactionProvider
/// [`ChainedContextProvider`]: crate::providers::ChainedContextProvider
pub struct SummarizingCompactionProvider {
    pub(super) inner: Arc<dyn ContextProvider>,
    pub(super) fallback: Arc<dyn ContextProvider>,
    pub(super) summarizer: Arc<dyn Summarizer>,
    pub(super) judge: Option<Arc<dyn ImportanceJudge>>,
    pub(super) policy: ContextPolicy,
    pub(super) context_window: Option<u64>,
    pub(super) keep_recent: usize,
    pub(super) retention: Retention,
    pub(super) states: States,
    pub(super) tasks: Mutex<Vec<JoinHandle<()>>>,
    pub(super) summaries_applied: AtomicU64,
}

/// Fewer new messages than this are not worth a model call yet.
pub(super) const MIN_CANDIDATES: usize = 2;

pub(super) const JUDGE_EXCERPT_CHARS: usize = 600;

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
            .lock_or_recover()
            .get(&first_message)
            .and_then(|state| state.summary.clone())
    }

    /// How many requests have had a summary applied.
    pub fn summaries_applied(&self) -> u64 {
        self.summaries_applied.load(Ordering::Relaxed)
    }

    /// Waits for every running background summarization (tests, shutdown).
    pub async fn wait_for_background(&self) {
        let handles = std::mem::take(&mut *self.tasks.lock_or_recover());
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
            let states = self.states.lock_or_recover();
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
        let states = self.states.lock_or_recover();
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
            let mut states = self.states.lock_or_recover();
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
        let mut tasks = self.tasks.lock_or_recover();
        tasks.retain(|task| !task.is_finished());
        tasks.push(handle);
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
