//! The background task that writes a summary, and the messages it reads and produces.

use super::Summarizer;
use rusty_runtime::locks::MutexExt;

use std::sync::Arc;

use rusty_protocol::ids::{MessageId, Timestamp};
use rusty_protocol::messages::{AgentMessage, ContentBlock, MessageRole};

use crate::compaction::SUMMARY_MARKER;
use crate::importance::{clip, Importance, ImportanceJudge, JudgeItem};

use crate::summary::provider::JUDGE_EXCERPT_CHARS;
use crate::summary::state::{Plan, States, SummaryState};

pub(super) async fn summarize_in_background(
    states: States,
    key: MessageId,
    summarizer: Arc<dyn Summarizer>,
    judge: Option<Arc<dyn ImportanceJudge>>,
    plan: Plan,
) {
    let with_state = |f: &mut dyn FnMut(&mut SummaryState)| {
        let mut states = states.lock_or_recover();
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

pub(super) fn summary_message(summary: &str, condensed: usize) -> AgentMessage {
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
pub(super) fn task_description(messages: &[AgentMessage]) -> String {
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
