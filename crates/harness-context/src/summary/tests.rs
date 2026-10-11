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
