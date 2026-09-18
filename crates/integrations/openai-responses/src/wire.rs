//! OpenAI Responses API wire types and incremental SSE normalization.
//!
//! Reference: the already-working TS implementation of this same protocol
//! (`src/harness/core/engine/directExecution.ts`'s `openai-responses` branch,
//! via `@earendil-works/pi-ai`'s `api/openai-responses.js` +
//! `api/openai-responses-shared.js`) -- ported here so OpenCode Zen's
//! GPT/Grok/Muse-Spark model family (which has no CORS support and so cannot
//! run from the browser at all) can execute from Rust instead.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use harness_model::events::{ModelError, ModelEvent, ModelResult};
use harness_protocol::ids::ToolCallId;
use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use harness_protocol::tools::ToolDescriptor;
use harness_protocol::usage::ModelUsage;
use serde::Serialize;

use crate::usage::{OpenAiResponsesUsageMapper, RawOpenAiResponsesUsage};

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct OpenAiResponsesRequest {
    pub model: String,
    pub input: Vec<ResponsesInputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ResponsesTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ResponsesReasoning>,
    pub stream: bool,
    /// Always `false` -- this client never relies on OpenAI's own
    /// server-side conversation state (no multi-turn `previous_response_id`
    /// chaining); the full transcript is resent every turn, same as every
    /// other integration in this workspace.
    pub store: bool,
}

/// The Responses API's `reasoning.effort` request param.
/// <https://platform.openai.com/docs/api-reference/responses/create>
#[derive(Debug, Clone, Serialize)]
pub struct ResponsesReasoning {
    pub effort: String,
}

/// Maps rusty-core's 3-level `ReasoningEffort` onto the Responses API's own
/// `"low"`/`"medium"`/`"high"` string values -- a 1:1 mapping, no clamping
/// needed (unlike the UI's own 5-level picker, already clamped down to these
/// three levels by `providerMapping.ts` before it ever reaches Rust).
pub fn reasoning_effort_to_responses(
    effort: harness_protocol::backend::ReasoningEffort,
) -> ResponsesReasoning {
    use harness_protocol::backend::ReasoningEffort;
    let effort = match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
    };
    ResponsesReasoning {
        effort: effort.to_string(),
    }
}

/// The minimum `max_output_tokens` the Responses API accepts.
/// <https://github.com/earendil-works/pi/issues/6265>
pub const RESPONSES_MIN_OUTPUT_TOKENS: u64 = 16;

/// One item of the Responses API's `input` array. Unlike Chat Completions'
/// flat `messages` array, `input` mixes plain `{role, content}` messages
/// with typed function-call/function-call-output items.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ResponsesInputItem {
    /// A `user`/`system` message -- no `type` tag, matching the Responses
    /// API's "easy input message" shape.
    InputMessage {
        role: String,
        content: Vec<ResponsesInputContentPart>,
    },
    /// An `assistant` message, which the Responses API represents as an
    /// explicitly-tagged output item even when fed back in as input.
    OutputMessage {
        #[serde(rename = "type")]
        kind: String,
        role: String,
        content: Vec<ResponsesOutputContentPart>,
    },
    FunctionCall {
        #[serde(rename = "type")]
        kind: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        #[serde(rename = "type")]
        kind: String,
        call_id: String,
        output: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesInputContentPart {
    InputText { text: String },
    InputImage { detail: String, image_url: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesOutputContentPart {
    OutputText {
        text: String,
        annotations: Vec<serde_json::Value>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponsesTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub strict: bool,
}

pub fn tool_descriptor_to_responses(tool: &ToolDescriptor) -> ResponsesTool {
    ResponsesTool {
        kind: "function".to_string(),
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.input_schema.clone(),
        strict: false,
    }
}

fn concat_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn content_blocks_to_responses_input(content: &[ContentBlock]) -> Vec<ResponsesInputContentPart> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(ResponsesInputContentPart::InputText {
                text: text.clone(),
            }),
            ContentBlock::Image { mime_type, data } => Some(ResponsesInputContentPart::InputImage {
                detail: "auto".to_string(),
                image_url: format!(
                    "data:{mime_type};base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(data)
                ),
            }),
            _ => None,
        })
        .collect()
}

/// Provider tool-call IDs (`"call_xxx"`) keyed by the harness-internal
/// `ToolCallId`, same role as `harness-integration-openai`'s own
/// `ProviderToolIds` -- lets a tool call minted while parsing a streamed
/// response be translated back to the ID the Responses API expects when
/// that call reappears in a later turn's history.
pub type ProviderToolIds = Arc<Mutex<HashMap<ToolCallId, String>>>;

fn agent_message_to_responses(
    message: &AgentMessage,
    tool_ids: &HashMap<ToolCallId, String>,
) -> Vec<ResponsesInputItem> {
    let provider_id_for = |call_id: &ToolCallId| -> String {
        tool_ids
            .get(call_id)
            .cloned()
            .unwrap_or_else(|| call_id.to_string())
    };

    match message.role {
        MessageRole::Tool => message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult { call_id, result } => {
                    Some(ResponsesInputItem::FunctionCallOutput {
                        kind: "function_call_output".to_string(),
                        call_id: provider_id_for(call_id),
                        output: result.output_preview.clone(),
                    })
                }
                _ => None,
            })
            .collect(),
        MessageRole::User => {
            let content = content_blocks_to_responses_input(&message.content);
            if content.is_empty() {
                vec![]
            } else {
                vec![ResponsesInputItem::InputMessage {
                    role: "user".to_string(),
                    content,
                }]
            }
        }
        MessageRole::Assistant => {
            let mut items = Vec::new();
            let text = concat_text(&message.content);
            if !text.is_empty() {
                items.push(ResponsesInputItem::OutputMessage {
                    kind: "message".to_string(),
                    role: "assistant".to_string(),
                    content: vec![ResponsesOutputContentPart::OutputText {
                        text,
                        annotations: vec![],
                    }],
                });
            }
            for block in &message.content {
                if let ContentBlock::ToolUse { call } = block {
                    items.push(ResponsesInputItem::FunctionCall {
                        kind: "function_call".to_string(),
                        id: None,
                        call_id: provider_id_for(&call.id),
                        name: call.name.clone(),
                        arguments: call.arguments.to_string(),
                    });
                }
            }
            items
        }
        MessageRole::System => unreachable!("system messages must be filtered before conversion"),
    }
}

/// Builds the system-role input item (if any), same precedence Anthropic's
/// and OpenAI's own clients use: prefer the request's `system_prompt` field,
/// falling back to concatenating any `System`-role messages in the
/// transcript.
pub fn build_system_message(
    system_prompt: &str,
    messages: &[AgentMessage],
) -> Option<ResponsesInputItem> {
    let text = if !system_prompt.is_empty() {
        Some(system_prompt.to_string())
    } else {
        let collected: Vec<&str> = messages
            .iter()
            .filter(|m| m.role == MessageRole::System)
            .flat_map(|m| m.content.iter())
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        (!collected.is_empty()).then(|| collected.join("\n"))
    };
    text.map(|content| ResponsesInputItem::InputMessage {
        role: "system".to_string(),
        content: vec![ResponsesInputContentPart::InputText { text: content }],
    })
}

pub fn convert_messages_with_tool_ids(
    messages: &[AgentMessage],
    tool_ids: &HashMap<ToolCallId, String>,
) -> Vec<ResponsesInputItem> {
    messages
        .iter()
        .filter(|m| m.role != MessageRole::System)
        .flat_map(|m| agent_message_to_responses(m, tool_ids))
        .collect()
}

// ---------------------------------------------------------------------------
// Streaming SSE parser
// ---------------------------------------------------------------------------

struct ToolBuffer {
    id: ToolCallId,
    name: String,
}

/// Stateful OpenAI Responses API SSE parser.
///
/// Frames are `event: <name>\ndata: {...}\n\n`; the JSON payload's own
/// `"type"` field always matches the `event:` line, so this parser reads
/// only the `data:` line's JSON and dispatches on that, exactly as
/// `harness-integration-openai`'s Chat Completions parser does with its own
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
    events: Vec<ModelEvent>,
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
            events: Vec::new(),
            tool_ids,
        }
    }

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

        self.events.extend(events.iter().cloned());
        Ok(events)
    }

    pub fn finish(&mut self) -> Result<(Vec<ModelEvent>, ModelResult), ModelError> {
        if !self.buffer.iter().all(u8::is_ascii_whitespace) {
            let trailing = std::mem::take(&mut self.buffer);
            let events = self.parse_block(&trailing)?;
            self.events.extend(events.clone());
        }
        if !self.saw_terminal {
            return Err(ModelError::StreamInterrupted {
                message: "Responses stream ended before a terminal response event".to_string(),
            });
        }

        let result = self
            .result
            .clone()
            .expect("saw_terminal is only set alongside self.result");
        Ok((vec![], result))
    }

    fn parse_block(&mut self, bytes: &[u8]) -> Result<Vec<ModelEvent>, ModelError> {
        let block = std::str::from_utf8(bytes).map_err(|error| ModelError::Protocol {
            message: error.to_string(),
        })?;
        let data = block
            .lines()
            .filter_map(|line| {
                line.trim_end_matches('\r')
                    .strip_prefix("data:")
                    .map(str::trim)
            })
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            return Ok(vec![]);
        }

        let value: serde_json::Value =
            serde_json::from_str(&data).map_err(|error| ModelError::Protocol {
                message: format!("invalid Responses SSE frame: {error}"),
            })?;

        let event_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match event_type {
            "response.output_text.delta" | "response.refusal.delta" => {
                let delta = value
                    .get("delta")
                    .and_then(|d| d.as_str())
                    .unwrap_or_default();
                if delta.is_empty() {
                    return Ok(vec![]);
                }
                let event = ModelEvent::TextDelta {
                    delta: delta.to_string(),
                };
                self.events.push(event.clone());
                Ok(vec![event])
            }
            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                let delta = value
                    .get("delta")
                    .and_then(|d| d.as_str())
                    .unwrap_or_default();
                if delta.is_empty() {
                    return Ok(vec![]);
                }
                let event = ModelEvent::ReasoningDelta {
                    delta: delta.to_string(),
                };
                self.events.push(event.clone());
                Ok(vec![event])
            }
            "response.output_item.added" => {
                let item = &value["item"];
                if item.get("type").and_then(|t| t.as_str()) != Some("function_call") {
                    return Ok(vec![]);
                }
                let output_index = value.get("output_index").and_then(|i| i.as_u64()).unwrap_or(0);
                let id = ToolCallId::new();
                if let Some(provider_call_id) = item.get("call_id").and_then(|c| c.as_str()) {
                    self.tool_ids
                        .lock()
                        .expect("provider tool-id map poisoned")
                        .insert(id, provider_call_id.to_string());
                }
                let name = item
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                self.tool_buffers.insert(
                    output_index,
                    ToolBuffer {
                        id,
                        name: name.clone(),
                    },
                );
                self.saw_function_call = true;
                let event = ModelEvent::ToolCallStarted { id, name };
                self.events.push(event.clone());
                Ok(vec![event])
            }
            "response.function_call_arguments.delta" => {
                let output_index = value.get("output_index").and_then(|i| i.as_u64()).unwrap_or(0);
                let Some(buffer) = self.tool_buffers.get(&output_index) else {
                    return Ok(vec![]);
                };
                let delta = value
                    .get("delta")
                    .and_then(|d| d.as_str())
                    .unwrap_or_default();
                if delta.is_empty() {
                    return Ok(vec![]);
                }
                let event = ModelEvent::ToolCallDelta {
                    id: buffer.id,
                    delta: delta.to_string(),
                };
                self.events.push(event.clone());
                Ok(vec![event])
            }
            "response.output_item.done" => {
                let item = &value["item"];
                if item.get("type").and_then(|t| t.as_str()) != Some("function_call") {
                    return Ok(vec![]);
                }
                let output_index = value.get("output_index").and_then(|i| i.as_u64()).unwrap_or(0);
                let Some(buffer) = self.tool_buffers.remove(&output_index) else {
                    return Ok(vec![]);
                };
                let arguments = item
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .unwrap_or("{}");
                let input = if arguments.is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str(arguments).map_err(|error| ModelError::Protocol {
                        message: format!("invalid tool arguments: {error}"),
                    })?
                };
                let event = ModelEvent::ToolCallCompleted {
                    id: buffer.id,
                    name: buffer.name,
                    input,
                };
                self.events.push(event.clone());
                Ok(vec![event])
            }
            "response.completed" | "response.incomplete" => {
                self.saw_terminal = true;
                let response = &value["response"];
                if let Some(model) = response.get("model").and_then(|m| m.as_str()) {
                    self.model = model.to_string();
                }
                if let Some(usage_value) = response.get("usage").filter(|v| !v.is_null()) {
                    if let Ok(raw) =
                        serde_json::from_value::<RawOpenAiResponsesUsage>(usage_value.clone())
                    {
                        self.usage = OpenAiResponsesUsageMapper::map_usage(&raw);
                    }
                }
                self.stop_reason = if self.saw_function_call {
                    "tool_calls".to_string()
                } else if event_type == "response.incomplete" {
                    "length".to_string()
                } else {
                    "stop".to_string()
                };

                let mut emitted = Vec::new();
                let usage_event = ModelEvent::UsageUpdate {
                    usage: self.usage.clone(),
                };
                emitted.push(usage_event.clone());
                self.events.push(usage_event);

                let result = ModelResult {
                    stop_reason: self.stop_reason.clone(),
                    usage: self.usage.clone(),
                    cost: OpenAiResponsesUsageMapper::calculate_cost(&self.usage, &self.model),
                };
                self.result = Some(result.clone());
                let completed_event = ModelEvent::Completed {
                    result: result.clone(),
                };
                emitted.push(completed_event.clone());
                self.events.push(completed_event);
                Ok(emitted)
            }
            "response.failed" => {
                self.saw_terminal = true;
                let response = &value["response"];
                let error = response.get("error");
                let details = response.get("incomplete_details");
                let message = error
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        details
                            .and_then(|d| d.get("reason"))
                            .and_then(|r| r.as_str())
                            .map(|reason| format!("incomplete: {reason}"))
                    })
                    .unwrap_or_else(|| "Unknown error (no error details in response)".to_string());
                let code = error
                    .and_then(|e| e.get("code"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("response_failed")
                    .to_string();
                Err(ModelError::BackendError { message, code })
            }
            "error" => {
                let message = value
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Responses stream error")
                    .to_string();
                let code = value
                    .get("code")
                    .and_then(|c| c.as_str())
                    .unwrap_or("stream_error")
                    .to_string();
                Err(ModelError::BackendError { message, code })
            }
            _ => Ok(vec![]),
        }
    }
}

fn find_sse_boundary(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|window| window == b"\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serializes_with_the_expected_field_names() {
        let request = OpenAiResponsesRequest {
            model: "gpt-5".to_string(),
            input: vec![ResponsesInputItem::InputMessage {
                role: "user".to_string(),
                content: vec![ResponsesInputContentPart::InputText {
                    text: "hi".to_string(),
                }],
            }],
            tools: None,
            max_output_tokens: Some(4096),
            temperature: Some(0.5),
            reasoning: None,
            stream: true,
            store: false,
        };
        let json = serde_json::to_value(&request).expect("serialize");
        assert_eq!(json["model"], "gpt-5");
        assert_eq!(json["max_output_tokens"], 4096);
        assert_eq!(json["temperature"], 0.5);
        assert_eq!(json["stream"], true);
        assert_eq!(json["store"], false);
        assert_eq!(json["input"][0]["role"], "user");
        assert_eq!(json["input"][0]["content"][0]["type"], "input_text");
        assert!(
            json["input"][0].get("type").is_none(),
            "a plain user input message must not carry a type tag"
        );
        assert!(
            json.get("reasoning").is_none(),
            "reasoning must be omitted, not null, when no effort was requested"
        );
    }

    #[test]
    fn reasoning_effort_maps_onto_the_responses_apis_own_effort_strings() {
        use harness_protocol::backend::ReasoningEffort;
        assert_eq!(reasoning_effort_to_responses(ReasoningEffort::Low).effort, "low");
        assert_eq!(reasoning_effort_to_responses(ReasoningEffort::Medium).effort, "medium");
        assert_eq!(reasoning_effort_to_responses(ReasoningEffort::High).effort, "high");
    }

    #[test]
    fn a_requested_reasoning_effort_serializes_into_the_request_body() {
        use harness_protocol::backend::ReasoningEffort;
        let request = OpenAiResponsesRequest {
            model: "gpt-5".to_string(),
            input: vec![],
            tools: None,
            max_output_tokens: None,
            temperature: None,
            reasoning: Some(reasoning_effort_to_responses(ReasoningEffort::Medium)),
            stream: true,
            store: false,
        };
        let json = serde_json::to_value(&request).expect("serialize");
        assert_eq!(json["reasoning"]["effort"], "medium");
    }

    #[test]
    fn function_call_item_serializes_in_the_responses_shape() {
        let item = ResponsesInputItem::FunctionCall {
            kind: "function_call".to_string(),
            id: None,
            call_id: "call_123".to_string(),
            name: "get_weather".to_string(),
            arguments: "{\"city\":\"paris\"}".to_string(),
        };
        let json = serde_json::to_value(&item).expect("serialize");
        assert_eq!(json["type"], "function_call");
        assert_eq!(json["call_id"], "call_123");
        assert_eq!(json["name"], "get_weather");
        assert!(json.get("id").is_none(), "omitted id must not serialize");
    }

    #[test]
    fn assistant_message_becomes_an_explicitly_tagged_output_message() {
        let json = serde_json::to_value(ResponsesInputItem::OutputMessage {
            kind: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![ResponsesOutputContentPart::OutputText {
                text: "hello".to_string(),
                annotations: vec![],
            }],
        })
        .expect("serialize");
        assert_eq!(json["type"], "message");
        assert_eq!(json["content"][0]["type"], "output_text");
        assert_eq!(json["content"][0]["text"], "hello");
    }

    fn frame(json: serde_json::Value) -> String {
        format!("data: {json}\n\n")
    }

    #[test]
    fn text_delta_and_completed_produce_the_expected_event_sequence() {
        let mut parser = OpenAiResponsesSseParser::new();
        let mut events = Vec::new();
        events.extend(
            parser
                .push_chunk(
                    frame(serde_json::json!({
                        "type": "response.output_text.delta",
                        "output_index": 0,
                        "delta": "hi"
                    }))
                    .as_bytes(),
                )
                .expect("valid frame"),
        );
        events.extend(
            parser
                .push_chunk(
                    frame(serde_json::json!({
                        "type": "response.completed",
                        "response": {
                            "model": "gpt-5",
                            "status": "completed",
                            "usage": {
                                "input_tokens": 2,
                                "output_tokens": 1,
                                "total_tokens": 3
                            }
                        }
                    }))
                    .as_bytes(),
                )
                .expect("valid frame"),
        );
        let (terminal, result) = parser.finish().expect("terminal event seen");
        events.extend(terminal);

        assert!(events
            .iter()
            .any(|e| matches!(e, ModelEvent::TextDelta { delta } if delta == "hi")));
        assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
        assert_eq!(result.stop_reason, "stop");
        assert_eq!(result.usage.input_tokens.value(), Some(2));
        assert_eq!(result.usage.output_tokens.value(), Some(1));
        assert_eq!(result.usage.total_tokens.value(), Some(3));
    }

    #[test]
    fn parser_rejects_a_stream_without_a_terminal_event() {
        let mut parser = OpenAiResponsesSseParser::new();
        parser
            .push_chunk(
                frame(serde_json::json!({
                    "type": "response.output_text.delta",
                    "output_index": 0,
                    "delta": "hi"
                }))
                .as_bytes(),
            )
            .expect("valid frame");
        let error = parser.finish().expect_err("must reject a stream missing a terminal event");
        assert!(matches!(error, ModelError::StreamInterrupted { .. }));
        assert!(error.is_retryable(), "a dropped connection, not malformed data, should be retried");
    }

    #[test]
    fn function_call_streams_started_delta_completed_and_marks_tool_calls_stop_reason() {
        let mut parser = OpenAiResponsesSseParser::new();
        let mut events = Vec::new();
        events.extend(
            parser
                .push_chunk(
                    frame(serde_json::json!({
                        "type": "response.output_item.added",
                        "output_index": 0,
                        "item": {
                            "type": "function_call",
                            "call_id": "call_abc",
                            "name": "get_weather",
                            "arguments": ""
                        }
                    }))
                    .as_bytes(),
                )
                .expect("valid frame"),
        );
        events.extend(
            parser
                .push_chunk(
                    frame(serde_json::json!({
                        "type": "response.function_call_arguments.delta",
                        "output_index": 0,
                        "delta": "{\"city\":\"paris\"}"
                    }))
                    .as_bytes(),
                )
                .expect("valid frame"),
        );
        events.extend(
            parser
                .push_chunk(
                    frame(serde_json::json!({
                        "type": "response.output_item.done",
                        "output_index": 0,
                        "item": {
                            "type": "function_call",
                            "call_id": "call_abc",
                            "name": "get_weather",
                            "arguments": "{\"city\":\"paris\"}"
                        }
                    }))
                    .as_bytes(),
                )
                .expect("valid frame"),
        );
        events.extend(
            parser
                .push_chunk(
                    frame(serde_json::json!({
                        "type": "response.completed",
                        "response": { "status": "completed" }
                    }))
                    .as_bytes(),
                )
                .expect("valid frame"),
        );
        let (terminal, result) = parser.finish().expect("terminal event seen");
        events.extend(terminal);

        assert!(events
            .iter()
            .any(|e| matches!(e, ModelEvent::ToolCallStarted { name, .. } if name == "get_weather")));
        let completed = events
            .iter()
            .find_map(|e| match e {
                ModelEvent::ToolCallCompleted { name, input, .. } => {
                    Some((name.clone(), input.clone()))
                }
                _ => None,
            })
            .expect("a ToolCallCompleted event");
        assert_eq!(completed.0, "get_weather");
        assert_eq!(completed.1, serde_json::json!({ "city": "paris" }));
        assert_eq!(result.stop_reason, "tool_calls");
    }

    #[test]
    fn response_failed_surfaces_as_backend_error() {
        let mut parser = OpenAiResponsesSseParser::new();
        let result = parser.push_chunk(
            frame(serde_json::json!({
                "type": "response.failed",
                "response": {
                    "error": { "code": "server_error", "message": "boom" }
                }
            }))
            .as_bytes(),
        );
        assert!(matches!(
            result,
            Err(ModelError::BackendError { code, .. }) if code == "server_error"
        ));
    }
}
