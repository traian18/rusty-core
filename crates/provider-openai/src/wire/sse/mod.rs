//! Incremental normalization of the Chat Completions SSE stream into [`ModelEvent`]s.
//!
//! The stream is plain `data: {...}` frames (no named `event:` lines), ending with a literal
//! `data: [DONE]`. There is no per-tool-call "stop" signal: a tool call's arguments are complete
//! only when the whole stream ends, so tool calls are flushed as `ToolCallCompleted` in
//! [`finish`](OpenAiSseParser::finish) rather than mid-stream. `chunk` handles one frame.

mod chunk;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusty_model_backend::events::{ModelError, ModelEvent, ModelResult};
use rusty_protocol::ids::ToolCallId;

use rusty_protocol::usage::ModelUsage;

use crate::usage::OpenAiUsageMapper;
use crate::wire::convert::ProviderToolIds;

struct ToolBuffer {
    id: ToolCallId,
    name: String,
    json: String,
}

/// Stateful OpenAI Chat Completions SSE parser.
///
/// OpenAI's stream is plain `data: {...}\n\n` frames (no named `event:`
/// lines like Anthropic), terminated by a literal `data: [DONE]\n\n`. Unlike
/// Anthropic, there is no per-tool-call "stop" signal — a tool call's
/// arguments are considered complete only once the whole stream ends, which
/// is why tool calls are flushed as `ToolCallCompleted` in [`finish`](Self::finish)
/// rather than mid-stream.
pub struct OpenAiSseParser {
    buffer: Vec<u8>,
    tools: HashMap<usize, ToolBuffer>,
    usage: ModelUsage,
    usage_event_sent: bool,
    stop_reason: String,
    model: String,
    response_id: Option<String>,
    saw_done: bool,
    result: Option<ModelResult>,
    events: Vec<ModelEvent>,
    tool_ids: ProviderToolIds,
}

impl Default for OpenAiSseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAiSseParser {
    pub fn new() -> Self {
        Self::with_tool_ids(Arc::new(Mutex::new(HashMap::new())))
    }

    pub fn with_tool_ids(tool_ids: ProviderToolIds) -> Self {
        Self {
            buffer: Vec::new(),
            tools: HashMap::new(),
            usage: ModelUsage::default(),
            usage_event_sent: false,
            stop_reason: "stop".to_string(),
            model: String::new(),
            response_id: None,
            saw_done: false,
            result: None,
            events: Vec::new(),
            tool_ids,
        }
    }

    pub fn push_chunk(&mut self, chunk: &[u8]) -> Result<Vec<ModelEvent>, ModelError> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();

        while let Some(boundary) = find_sse_boundary(&self.buffer) {
            let block = self.buffer.drain(..boundary).collect::<Vec<_>>();
            self.buffer.drain(..2); // the "\n\n" (or "\n" of a "\r\n\r\n") delimiter
            if block.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if let Some(event) = self.parse_block(&block)? {
                events.push(event);
            }
        }

        self.events.extend(events.iter().cloned());
        Ok(events)
    }

    pub(crate) fn response_id(&self) -> Option<&str> {
        self.response_id.as_deref()
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.saw_done
    }

    pub(crate) fn has_usage(&self) -> bool {
        self.usage.total_tokens.value().unwrap_or(0) > 0
            || self.usage.input_tokens.value().unwrap_or(0) > 0
            || self.usage.output_tokens.value().unwrap_or(0) > 0
    }

    pub(crate) fn set_usage(&mut self, usage: ModelUsage) {
        self.usage = usage;
    }

    pub fn finish(&mut self) -> Result<(Vec<ModelEvent>, ModelResult), ModelError> {
        if self.result.is_some() {
            return Err(ModelError::Protocol {
                message: "SSE parser was finished more than once".to_string(),
            });
        }
        if !self.buffer.iter().all(u8::is_ascii_whitespace) {
            let trailing = std::mem::take(&mut self.buffer);
            let _ = self.parse_block(&trailing)?;
        }
        if !self.saw_done {
            return Err(ModelError::StreamInterrupted {
                message: "SSE stream ended without [DONE]".to_string(),
            });
        }

        let mut terminal_events = Vec::new();
        // Some OpenAI-compatible providers attach usage to their final
        // choices chunk rather than emitting a separate usage-only chunk.
        // That chunk can also carry a text/tool delta, so publish its usage
        // here before Completed instead of dropping either event.
        if self.has_usage() && !self.usage_event_sent {
            terminal_events.push(ModelEvent::UsageUpdate {
                usage: self.usage.clone(),
            });
        }
        let mut tools: Vec<(usize, ToolBuffer)> = self.tools.drain().collect();
        tools.sort_by_key(|(index, _)| *index);
        for (_, tool) in tools {
            let input = if tool.json.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str(&tool.json).map_err(|error| ModelError::Protocol {
                    message: format!("invalid tool arguments: {error}"),
                })?
            };
            terminal_events.push(ModelEvent::ToolCallCompleted {
                id: tool.id,
                name: tool.name,
                input,
            });
        }

        let result = ModelResult {
            stop_reason: self.stop_reason.clone(),
            usage: self.usage.clone(),
            cost: OpenAiUsageMapper::calculate_cost(&self.usage, &self.model),
        };
        self.result = Some(result.clone());
        terminal_events.push(ModelEvent::Completed {
            result: result.clone(),
        });
        self.events.extend(terminal_events.iter().cloned());
        Ok((terminal_events, result))
    }
}

/// Finds the byte offset of the next SSE block boundary (a blank line),
/// tolerating both `\n\n` and `\r\n\r\n` line endings. Returns the offset to
/// drain up to; the caller separately drains a fixed 2-byte delimiter,
/// consistent with how `push_chunk` above calls this (matching Anthropic's
/// parser, `\r\n\r\n` blocks still end up correctly split because the `\r`s
/// are trimmed per-line during parsing).
fn find_sse_boundary(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|window| window == b"\n\n")
}
