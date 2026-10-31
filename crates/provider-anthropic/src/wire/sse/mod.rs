//! Incremental normalization of the Anthropic SSE stream into [`ModelEvent`]s.
//!
//! The parser accepts arbitrary HTTP byte chunks and returns events as soon as a complete SSE
//! block arrives. Partial UTF-8 and JSON fragments stay buffered until their terminating blank
//! line, so callers never need to align transport chunks to SSE events. `block` handles one
//! block, `non_streaming` the case where a gateway answers with a single JSON message, and
//! `usage` the token counts.

mod block;
mod boundary;
mod fields;
mod non_streaming;
mod usage;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusty_model_backend::events::{ModelError, ModelEvent, ModelResult};
use rusty_protocol::ids::ToolCallId;

use rusty_protocol::usage::ModelUsage;

use self::boundary::find_sse_boundary;
use crate::usage::AnthropicUsageMapper;
use crate::wire::convert::ProviderToolIds;

/// A tool call being assembled from `input_json_delta` fragments.
struct ToolBuffer {
    id: ToolCallId,
    name: String,
    json: String,
}

/// Stateful Anthropic SSE parser that accepts arbitrary HTTP byte chunks.
///
/// Events are returned as soon as a complete SSE block arrives. Partial UTF-8
/// and JSON fragments remain buffered until their terminating blank line is
/// received, so callers never need to align transport chunks to SSE events.
pub struct AnthropicSseParser {
    buffer: Vec<u8>,
    tools: HashMap<usize, ToolBuffer>,
    usage: ModelUsage,
    stop_reason: String,
    model: String,
    saw_message_stop: bool,
    result: Option<ModelResult>,
    events: Vec<ModelEvent>,
    pending_non_streaming_events: Vec<ModelEvent>,
    tool_ids: ProviderToolIds,
    /// Indices of content blocks belonging to the synthetic structured-output
    /// tool. Their `input_json_delta` fragments are re-emitted as
    /// `TextDelta` and they produce no tool-call events, so a caller that
    /// asked for JSON sees assistant text — not a tool call it never
    /// registered and could not execute.
    structured_output_blocks: std::collections::HashSet<usize>,
}

impl Default for AnthropicSseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicSseParser {
    pub fn new() -> Self {
        Self::with_tool_ids(Arc::new(Mutex::new(HashMap::new())))
    }

    pub fn with_tool_ids(tool_ids: ProviderToolIds) -> Self {
        Self {
            buffer: Vec::new(),
            tools: HashMap::new(),
            usage: ModelUsage::default(),
            stop_reason: "end_turn".to_string(),
            model: String::new(),
            saw_message_stop: false,
            result: None,
            events: Vec::new(),
            pending_non_streaming_events: Vec::new(),
            tool_ids,
            structured_output_blocks: std::collections::HashSet::new(),
        }
    }

    /// Parse all complete SSE blocks currently available in `chunk`.
    pub fn push_chunk(&mut self, chunk: &[u8]) -> Result<Vec<ModelEvent>, ModelError> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();

        while let Some((boundary, delimiter_len)) = find_sse_boundary(&self.buffer) {
            let block = self.buffer.drain(..boundary).collect::<Vec<_>>();
            self.buffer.drain(..delimiter_len);
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

    /// Finish the stream, validating termination and producing the final result.
    pub fn finish(&mut self) -> Result<(Vec<ModelEvent>, ModelResult), ModelError> {
        if self.result.is_some() {
            return Err(ModelError::Protocol {
                message: "SSE parser was finished more than once".to_string(),
            });
        }

        if !self.buffer.iter().all(u8::is_ascii_whitespace) {
            let trailing = std::mem::take(&mut self.buffer);

            // Handle gateways/proxies that return a complete non-streaming
            // JSON message response ({"type":"message", ...}) instead of SSE frames.
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&trailing) {
                if value.get("type").and_then(|t| t.as_str()) == Some("message") {
                    let events = self.parse_non_streaming_json(&value);
                    self.pending_non_streaming_events.extend(events.clone());
                    self.events.extend(events);
                    self.saw_message_stop = true;
                }
            }

            if !self.saw_message_stop {
                if let Some(event) = self.parse_block(&trailing)? {
                    if !matches!(event, ModelEvent::Completed { .. }) {
                        return Err(ModelError::Protocol {
                            message: "SSE stream ended with an unterminated event".to_string(),
                        });
                    }
                }
            }
        }

        if !self.saw_message_stop {
            return Err(ModelError::StreamInterrupted {
                message: "SSE stream ended without message_stop".to_string(),
            });
        }

        let cost = AnthropicUsageMapper::calculate_cost(&self.usage, &self.model);
        let result = ModelResult {
            stop_reason: self.stop_reason.clone(),
            usage: self.usage.clone(),
            cost,
        };
        self.result = Some(result.clone());
        let terminal = ModelEvent::Completed {
            result: result.clone(),
        };
        self.events.push(terminal.clone());
        let mut terminal_events = std::mem::take(&mut self.pending_non_streaming_events);
        terminal_events.push(terminal);
        Ok((terminal_events, result))
    }

    /// Parse a complete recorded fixture in one call.
    pub fn parse_all(bytes: &[u8]) -> Result<Self, ModelError> {
        let mut parser = Self::new();
        let _ = parser.push_chunk(bytes)?;
        let _ = parser.finish()?;
        Ok(parser)
    }

    /// Return all normalized events after [`parse_all`](Self::parse_all).
    pub fn into_events(self) -> Vec<ModelEvent> {
        self.events
    }

    /// Return the final normalized result after [`parse_all`](Self::parse_all).
    pub fn into_result(self) -> Option<ModelResult> {
        self.result
    }
}
