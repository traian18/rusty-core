//! Replaces file reads that a later read of the same file made redundant.
//!
//! An agent that works on a file reads it again and again; every copy stays
//! in the history and is sent with each later call. A copy is redundant once
//! the same file was read again with the same arguments, or whole: the newer
//! copy is what counts. This provider swaps redundant copies for a one-line
//! stub.
//!
//! It changes the history only at fixed points, so a provider's prompt cache
//! keeps matching between them: the history is cut into steps of
//! [`STEP_CHARS`] (measured on the original messages), and only reads that
//! were superseded before the last step boundary are replaced. Until the next
//! boundary is crossed, every request sends the same prefix.

use std::collections::HashMap;

use async_trait::async_trait;
use harness_protocol::backend::ExecutionRequest;
use harness_protocol::ids::ToolCallId;
use harness_protocol::messages::{AgentMessage, ContentBlock};
use harness_runtime::traits::Workspace;
use serde_json::Value;

use crate::provider::ContextProvider;

/// How much history (in characters) passes between two rewrites.
pub const STEP_CHARS: usize = 120_000;

/// The tools whose results are file reads.
const READ_TOOLS: &[&str] = &["read_file", "fs.read"];

/// Replaces superseded reads at fixed points of the history (see the module docs).
#[derive(Debug, Default, Clone, Copy)]
pub struct SupersededReadsProvider;

#[async_trait]
impl ContextProvider for SupersededReadsProvider {
    async fn assemble(
        &self,
        mut request: ExecutionRequest,
        _workspace: &dyn Workspace,
    ) -> ExecutionRequest {
        let upto = rewrite_point(&request.messages, STEP_CHARS);
        stub_superseded_reads(&mut request.messages, upto);
        request
    }
}

/// The end (exclusive) of the history that may be rewritten: the first
/// message past the last whole step of `step` characters. 0 while the history
/// is shorter than one step.
pub fn rewrite_point(messages: &[AgentMessage], step: usize) -> usize {
    let sizes: Vec<usize> = messages.iter().map(size).collect();
    let total: usize = sizes.iter().sum();
    let boundary = (total / step) * step;
    if boundary == 0 {
        return 0;
    }
    let mut seen = 0;
    for (index, size) in sizes.iter().enumerate() {
        seen += size;
        if seen >= boundary {
            return index + 1;
        }
    }
    messages.len()
}

/// Replaces read results before `upto` that a later read before `upto`
/// supersedes. Returns how many were replaced.
pub fn stub_superseded_reads(messages: &mut [AgentMessage], upto: usize) -> usize {
    let upto = upto.min(messages.len());
    let reads: HashMap<ToolCallId, Read> = messages[..upto]
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { call } if READ_TOOLS.contains(&call.name.as_str()) => {
                Read::of(&call.arguments).map(|read| (call.id, read))
            }
            _ => None,
        })
        .collect();
    // Where each read's result is, in order.
    let mut order: Vec<(usize, ToolCallId)> = Vec::new();
    for (index, message) in messages[..upto].iter().enumerate() {
        for block in &message.content {
            if let ContentBlock::ToolResult { call_id, result } = block {
                if reads.contains_key(call_id) && !result.has_error {
                    order.push((index, *call_id));
                }
            }
        }
    }
    let superseded: Vec<ToolCallId> = order
        .iter()
        .enumerate()
        .filter(|(at, (_, id))| {
            let read = &reads[id];
            order[at + 1..]
                .iter()
                .any(|(_, later)| reads[later].supersedes(read))
        })
        .map(|(_, (_, id))| *id)
        .collect();
    let mut replaced = 0;
    for message in &mut messages[..upto] {
        for block in &mut message.content {
            if let ContentBlock::ToolResult { call_id, result } = block {
                if superseded.contains(call_id) && !result.output_preview.starts_with(STUB) {
                    result.output_preview = format!(
                        "{STUB} {} was read again later; use that newer copy]",
                        reads[call_id].path
                    );
                    replaced += 1;
                }
            }
        }
    }
    replaced
}

const STUB: &str = "[older read replaced:";

/// What a read asked for.
#[derive(Debug, PartialEq)]
struct Read {
    path: String,
    /// The rest of the arguments (line range, request); empty for a whole read.
    scope: String,
}

impl Read {
    fn of(arguments: &Value) -> Option<Self> {
        let fields = arguments.as_object()?;
        let path = fields
            .get("path")
            .or_else(|| fields.get("file_path"))?
            .as_str()?
            .trim_start_matches("./")
            .to_owned();
        let mut rest: Vec<(&String, &Value)> = fields
            .iter()
            .filter(|(key, _)| key.as_str() != "path" && key.as_str() != "file_path")
            .collect();
        rest.sort_by(|a, b| a.0.cmp(b.0));
        let scope = rest
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("&");
        Some(Self { path, scope })
    }

    /// A later read covers an earlier one of the same file when it asked the
    /// same thing or read the whole file.
    fn supersedes(&self, earlier: &Read) -> bool {
        self.path == earlier.path && (self.scope.is_empty() || self.scope == earlier.scope)
    }
}

fn size(message: &AgentMessage) -> usize {
    message
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::ToolUse { call } => call.name.len() + call.arguments.to_string().len(),
            ContentBlock::ToolResult { result, .. } => result.output_preview.len(),
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests;
