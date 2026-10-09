//! Pure message-list compaction primitives shared by the compaction providers.
//!
//! Three tiers, cheapest first:
//! 1. [`elide_tool_results`] shrinks bulky old tool output to a short stub.
//! 2. [`drop_prefix`] drops the oldest messages, only at tool-pair-safe cut
//!    points.
//! 3. Pinned messages (and optionally the first user message) survive both.

use std::collections::{HashMap, HashSet};

use harness_protocol::ids::{MessageId, Timestamp, ToolCallId};
use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};

/// Tool results at or below this size are left alone — a stub would not save much.
const ELIDE_MIN_CHARS: usize = 256;
/// Head of the original output kept in the stub so the gist survives.
const ELIDE_HEAD_CHARS: usize = 120;

/// Messages that must survive compaction regardless of age.
#[derive(Debug, Clone, Default)]
pub struct Retention {
    pub pinned: HashSet<MessageId>,
    /// Also keep the first user message (normally the original task).
    pub anchor_first_user: bool,
}

impl Retention {
    pub(crate) fn is_protected(&self, messages: &[AgentMessage], index: usize) -> bool {
        let message = &messages[index];
        if self.pinned.contains(&message.id) || is_summary(message) {
            return true;
        }
        self.anchor_first_user
            && message.role == MessageRole::User
            && !messages[..index]
                .iter()
                .any(|earlier| earlier.role == MessageRole::User)
    }
}

/// Opening text of every summary message. A summary already stands for the
/// history it replaced, so no tier may drop or elide it.
pub(crate) const SUMMARY_MARKER: &str = "[summary of earlier conversation";

fn is_summary(message: &AgentMessage) -> bool {
    message.role == MessageRole::System
        && matches!(message.content.first(), Some(ContentBlock::Text { text }) if text.starts_with(SUMMARY_MARKER))
}

/// Replaces large tool results outside the last `protect_tail` messages with a
/// stub naming the tool and size. Returns how many results were elided.
pub fn elide_tool_results(
    messages: &mut [AgentMessage],
    protect_tail: usize,
    retention: &Retention,
) -> usize {
    let names: HashMap<ToolCallId, String> = messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { call } => Some((call.id, call.name.clone())),
            _ => None,
        })
        .collect();

    let end = messages.len().saturating_sub(protect_tail);
    let mut elided = 0;
    for index in 0..end {
        if retention.is_protected(messages, index) {
            continue;
        }
        for block in &mut messages[index].content {
            let ContentBlock::ToolResult { call_id, result } = block else {
                continue;
            };
            let len = result.output_preview.len();
            if len <= ELIDE_MIN_CHARS {
                continue;
            }
            let tool = names.get(call_id).map_or("tool", String::as_str);
            let head: String = result
                .output_preview
                .chars()
                .take(ELIDE_HEAD_CHARS)
                .collect();
            result.output_preview = format!(
                "[{tool} result elided to save context — {len} chars{}] {head}…",
                if result.has_error { ", error" } else { "" }
            );
            elided += 1;
        }
    }
    elided
}

fn tool_use_ids(message: &AgentMessage) -> impl Iterator<Item = ToolCallId> + '_ {
    message.content.iter().filter_map(|block| match block {
        ContentBlock::ToolUse { call } => Some(call.id),
        _ => None,
    })
}

fn tool_result_ids(message: &AgentMessage) -> impl Iterator<Item = ToolCallId> + '_ {
    message.content.iter().filter_map(|block| match block {
        ContentBlock::ToolResult { call_id, .. } => Some(*call_id),
        _ => None,
    })
}

/// Keeps the last `keep` messages plus protected ones, dropping the rest and
/// inserting `note(dropped)` where the gap is. The kept set is closed over tool
/// pairs: a kept `ToolResult` always keeps its `ToolUse` message and vice versa,
/// so the cut point moves earlier rather than orphaning either half.
///
/// Returns the number of messages dropped (0 means `messages` is unchanged).
pub fn drop_prefix(
    messages: &mut Vec<AgentMessage>,
    keep: usize,
    retention: &Retention,
    note: impl FnOnce(usize) -> AgentMessage,
) -> usize {
    let len = messages.len();
    if len <= keep {
        return 0;
    }
    let mut kept = vec![false; len];
    for (index, flag) in kept.iter_mut().enumerate() {
        *flag = index >= len - keep || retention.is_protected(messages, index);
    }

    // Close the kept set over tool pairs until stable.
    let mut owner_of_use: HashMap<ToolCallId, usize> = HashMap::new();
    let mut owner_of_result: HashMap<ToolCallId, usize> = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        owner_of_use.extend(tool_use_ids(message).map(|id| (id, index)));
        owner_of_result.extend(tool_result_ids(message).map(|id| (id, index)));
    }
    loop {
        let mut changed = false;
        for index in 0..len {
            if !kept[index] {
                continue;
            }
            let partners = tool_result_ids(&messages[index])
                .filter_map(|id| owner_of_use.get(&id))
                .chain(tool_use_ids(&messages[index]).filter_map(|id| owner_of_result.get(&id)));
            for &partner in partners {
                if !kept[partner] {
                    kept[partner] = true;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    let dropped = kept.iter().filter(|k| !**k).count();
    if dropped == 0 {
        return 0;
    }

    // Place the note after the last kept message that precedes the first
    // retained tail message; simplest stable rule: before the first kept
    // message that follows a dropped one.
    let mut out = Vec::with_capacity(len - dropped + 1);
    let mut note = Some(note);
    let mut pending_gap = false;
    for (message, keep_it) in std::mem::take(messages).into_iter().zip(kept) {
        if !keep_it {
            pending_gap = true;
            continue;
        }
        if pending_gap {
            if let Some(make) = note.take() {
                out.push(make(dropped));
            }
            pending_gap = false;
        }
        out.push(message);
    }
    if let Some(make) = note.take() {
        out.push(make(dropped));
    }
    *messages = out;
    dropped
}

/// Builds the synthetic system note marking dropped history.
pub fn truncation_note(dropped: usize) -> AgentMessage {
    AgentMessage {
        id: MessageId::new(),
        role: MessageRole::System,
        content: vec![ContentBlock::Text {
            text: format!("[earlier conversation truncated — {dropped} message(s) omitted]"),
        }],
        created_at: Timestamp::now(),
    }
}
