//! Incremental normalization of the Responses API SSE stream into [`ModelEvent`]s.
//!
//! Frames are `event: <name>\ndata: {...}\n\n`. The payload's own `"type"` always matches the
//! `event:` line, so the parser reads only the `data:` lines and dispatches on that. Unlike Chat
//! Completions there is no `[DONE]` sentinel: the stream is complete once a terminal
//! `response.completed`, `.incomplete` or `.failed` event (or a top-level `error`) has been seen,
//! and ending without one is a protocol error. `frame` handles one frame.

mod frame;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusty_model_backend::events::{ModelError, ModelEvent, ModelResult};
use rusty_protocol::ids::ToolCallId;

use rusty_protocol::usage::ModelUsage;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------
use crate::wire::convert::ProviderToolIds;

/// A function call being assembled from argument deltas.
struct ToolBuffer {
    id: ToolCallId,
    name: String,
}

/// Stateful OpenAI Responses API SSE parser.
///
/// Frames are `event: <name>\ndata: {...}\n\n`; the JSON payload's own
/// `"type"` field always matches the `event:` line, so this parser reads
/// only the `data:` line's JSON and dispatches on that, exactly as
/// `rusty-provider-openai`'s Chat Completions parser does with its own
/// (unnamed) frames. Unlike Chat Completions, there is no `[DONE]` sentinel
/// -- the stream is considered complete once a terminal `response.completed`
/// / `.incomplete` / `.failed` event (or a top-level `error` event) has been
/// seen; ending without one is a protocol error.
pub struct OpenAiResponsesSseParser {
    buffer: Vec<u8>,
    tool_buffers: HashMap<u64, ToolBuffer>,
    saw_function_call: bool,
    usage: ModelUsage,
    stop_reason: String,
    model: String,
    saw_terminal: bool,
    result: Option<ModelResult>,
    tool_ids: ProviderToolIds,
}

impl Default for OpenAiResponsesSseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAiResponsesSseParser {
    pub fn new() -> Self {
        Self::with_tool_ids(Arc::new(Mutex::new(HashMap::new())))
    }

    pub fn with_tool_ids(tool_ids: ProviderToolIds) -> Self {
        Self {
            buffer: Vec::new(),
            tool_buffers: HashMap::new(),
            saw_function_call: false,
            usage: ModelUsage::default(),
            stop_reason: "stop".to_string(),
            model: String::new(),
            saw_terminal: false,
            result: None,
            tool_ids,
        }
    }

    /// Parses every complete frame now available in `chunk`.
    pub fn push_chunk(&mut self, chunk: &[u8]) -> Result<Vec<ModelEvent>, ModelError> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(boundary) = find_sse_boundary(&self.buffer) {
            let block = self.buffer.drain(..boundary).collect::<Vec<_>>();
            self.buffer.drain(..2);
            if block.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            events.extend(self.parse_block(&block)?);
        }
        Ok(events)
    }

    /// Ends the stream: parses any trailing frame and requires a terminal event.
    pub fn finish(&mut self) -> Result<(Vec<ModelEvent>, ModelResult), ModelError> {
        if !self.buffer.iter().all(u8::is_ascii_whitespace) {
            let trailing = std::mem::take(&mut self.buffer);
            self.parse_block(&trailing)?;
        }
        match self.result.clone() {
            Some(result) if self.saw_terminal => Ok((vec![], result)),
            _ => Err(ModelError::StreamInterrupted {
                message: "Responses stream ended before a terminal response event".to_string(),
            }),
        }
    }
}

fn find_sse_boundary(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|window| window == b"\n\n")
}
