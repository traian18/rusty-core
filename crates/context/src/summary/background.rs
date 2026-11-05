//! The background task that writes a summary, and the messages it reads and produces.

use super::Summarizer;
use rusty_runtime::locks::MutexExt;

use std::sync::Arc;

use rusty_protocol::ids::{MessageId, Timestamp};
use rusty_protocol::messages::{AgentMessage, ContentBlock, MessageRole};

use crate::compaction::SUMMARY_MARKER;
use crate::importance::{clip, Importance, ImportanceJudge, JudgeItem};

use crate::observability::{CompactionPhase, CompactionTelemetry};
use crate::summary::provider::JUDGE_EXCERPT_CHARS;
use crate::summary::state::{Plan, States, SummaryState};

pub(super) async fn summarize_in_background(
    states: States,
    key: MessageId,
    summarizer: Arc<dyn Summarizer>,
    judge: Option<Arc<dyn ImportanceJudge>>,
    plan: Plan,
    telemetry: CompactionTelemetry,
) {
    let with_state = |f: &mut dyn FnMut(&mut SummaryState)| {
        let mut states = states.lock_or_recover();
        f(states.entry(key).or_default());
    };

    judge_candidates(&states, key, judge, &plan, &telemetry).await;

    let (to_summarize, phase) = classified_candidates(&states, key, &plan);
    telemetry.update(phase, None, None);
    let summary = if to_summarize.is_empty() {
        plan.previous
    } else {
        telemetry.update(
            CompactionPhase::Summarizing {
                messages: to_summarize.len(),
            },
            None,
            None,
        );
        match summarizer
            .summarize(plan.previous.as_deref(), &to_summarize)
            .await
        {
            Ok(text) => Some(text),
            // Leave the state untouched: the cheap tiers cover this request
            // and the next soft-pressure request retries.
            Err(_) => {
                telemetry.update(CompactionPhase::Failed, None, None);
                return;
            }
        }
    };
    with_state(&mut |state| {
        state.summary = summary.clone();
        state.covered_through = Some(plan.boundary);
        state.telemetry = Some(telemetry.clone());
    });
    telemetry.update(CompactionPhase::Ready, None, None);
}

fn classified_candidates(
    states: &States,
    key: MessageId,
    plan: &Plan,
) -> (Vec<AgentMessage>, CompactionPhase) {
    let mut states = states.lock_or_recover();
    let state = states.entry(key).or_default();
    let mut essential = 0;
    let mut disposable = 0;
    let mut useful = Vec::new();
    for message in &plan.candidates {
        if state.essential.contains(&message.id) {
            essential += 1;
        } else if state.disposable.contains(&message.id) {
            disposable += 1;
        } else {
            useful.push(message.clone());
        }
    }
    let phase = CompactionPhase::Judged {
        essential,
        useful: useful.len(),
        disposable,
    };
    (useful, phase)
}

async fn judge_candidates(
    states: &States,
    key: MessageId,
    judge: Option<Arc<dyn ImportanceJudge>>,
    plan: &Plan,
    telemetry: &CompactionTelemetry,
) {
    let Some(judge) = judge else {
        return;
    };
    let items: Vec<_> = {
        let states = states.lock_or_recover();
        let state = states.get(&key);
        plan.candidates
            .iter()
            .filter(|m| !state.is_some_and(|s| s.judged.contains(&m.id)))
            .map(|m| JudgeItem::from_message(m, JUDGE_EXCERPT_CHARS))
            .collect()
    };
    telemetry.update(
        CompactionPhase::Judging {
            candidates: items.len(),
        },
        None,
        None,
    );
    let Ok(verdicts) = judge.judge(&plan.task_text, &items).await else {
        telemetry.update(CompactionPhase::JudgeFailed, None, None);
        return;
    };
    let mut states = states.lock_or_recover();
    let state = states.entry(key).or_default();
    for verdict in verdicts {
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
