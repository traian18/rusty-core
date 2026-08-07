//! Anthropic request wire types and incremental SSE normalization.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use harness_model::events::{ModelError, ModelEvent, ModelResult};
use harness_protocol::ids::ToolCallId;
use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use harness_protocol::tools::ToolDescriptor;
use harness_protocol::usage::ModelUsage;
use serde::{Deserialize, Serialize};

use crate::usage::{AnthropicUsageMapper, RawAnthropicUsage};

#[derive(Debug, Clone, Serialize)]
pub struct AnthropicRequest {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
    pub max_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<AnthropicThinking>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnthropicMessage {
    pub role: AnthropicRole,
    pub content: Vec<AnthropicContentBlock>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnthropicRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
    #[serde(rename = "image")]
    Image { source: AnthropicImageSource },
}

/// Anthropic's Messages API image content block wire shape:
/// `{"type":"image","source":{"type":"base64","media_type":"image/png","data":"..."}}`
/// <https://docs.anthropic.com/en/api/messages> (vision).
#[derive(Debug, Clone, Serialize)]
pub struct AnthropicImageSource {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnthropicTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnthropicThinking {
    #[serde(rename = "type")]
    pub kind: String,
    pub budget_tokens: u64,
}

pub fn agent_message_to_anthropic(message: &AgentMessage) -> AnthropicMessage {
    let role = match message.role {
        MessageRole::User | MessageRole::Tool => AnthropicRole::User,
        MessageRole::Assistant => AnthropicRole::Assistant,
        MessageRole::System => panic!("system messages must be filtered before conversion"),
    };
    let content = message
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => AnthropicContentBlock::Text { text: text.clone() },
            ContentBlock::ToolUse { call } => AnthropicContentBlock::ToolUse {
                id: call.id.to_string(),
                name: call.name.clone(),
                input: call.arguments.clone(),
            },
            ContentBlock::ToolResult { call_id, result } => AnthropicContentBlock::ToolResult {
                tool_use_id: call_id.to_string(),
                content: result.output_preview.clone(),
            },
            ContentBlock::Image { mime_type, data } => AnthropicContentBlock::Image {
                source: AnthropicImageSource {
                    kind: "base64",
                    media_type: mime_type.clone(),
                    data: base64::engine::general_purpose::STANDARD.encode(data),
                },
            },
        })
        .collect();
    AnthropicMessage { role, content }
}

pub fn tool_descriptor_to_anthropic(tool: &ToolDescriptor) -> AnthropicTool {
    AnthropicTool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        input_schema: tool.input_schema.clone(),
    }
}

pub fn build_system(messages: &[AgentMessage]) -> Option<String> {
    let text: Vec<&str> = messages
        .iter()
        .filter(|message| message.role == MessageRole::System)
        .flat_map(|message| message.content.iter())
        .filter_map(|block| {
            if let ContentBlock::Text { text } = block {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    (!text.is_empty()).then(|| text.join("\n"))
}

pub fn convert_messages(messages: &[AgentMessage]) -> Vec<AnthropicMessage> {
    messages
        .iter()
        .filter(|message| message.role != MessageRole::System)
        .map(agent_message_to_anthropic)
        .collect()
}

/// Provider tool IDs keyed by the harness-internal call ID.
pub type ProviderToolIds = Arc<Mutex<HashMap<ToolCallId, String>>>;

/// Convert messages while restoring provider-issued tool IDs on follow-up turns.
pub fn convert_messages_with_tool_ids(
    messages: &[AgentMessage],
    tool_ids: &HashMap<ToolCallId, String>,
) -> Vec<AnthropicMessage> {
    let mut converted = convert_messages(messages);
    for message in &mut converted {
        for block in &mut message.content {
            let wire_id = match block {
                AnthropicContentBlock::ToolUse { id, .. } => Some(id),
                AnthropicContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id),
                AnthropicContentBlock::Text { .. } | AnthropicContentBlock::Image { .. } => None,
            };
            if let Some(wire_id) = wire_id {
                if let Ok(internal_id) = wire_id.parse::<ToolCallId>() {
                    if let Some(provider_id) = tool_ids.get(&internal_id) {
                        *wire_id = provider_id.clone();
                    }
                }
            }
        }
    }
    converted
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    #[serde(alias = "cache_creation_input_tokens")]
    cache_write_input_tokens: Option<u64>,
}

#[derive(Default)]
struct ToolBuffer {
    id: Option<ToolCallId>,
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
    tool_ids: ProviderToolIds,
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
            tool_ids,
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
            if let Some(event) = self.parse_block(&trailing)? {
                if !matches!(event, ModelEvent::Completed { .. }) {
                    return Err(ModelError::Protocol {
                        message: "SSE stream ended with an unterminated event".to_string(),
                    });
                }
            }
        }

        if !self.saw_message_stop {
            return Err(ModelError::Protocol {
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
        Ok((vec![terminal], result))
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

    fn parse_block(&mut self, bytes: &[u8]) -> Result<Option<ModelEvent>, ModelError> {
        let block = std::str::from_utf8(bytes).map_err(|error| ModelError::Protocol {
            message: error.to_string(),
        })?;
        let event_type = block.lines().find_map(|line| {
            line.trim_end_matches('\r')
                .strip_prefix("event:")
                .map(str::trim)
        });
        let data = block
            .lines()
            .filter_map(|line| {
                line.trim_end_matches('\r')
                    .strip_prefix("data:")
                    .map(str::trim)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let Some(kind) = event_type else {
            return Ok(None);
        };
        if data.is_empty() {
            return Ok(None);
        }
        let value: serde_json::Value =
            serde_json::from_str(&data).map_err(|error| ModelError::Protocol {
                message: format!("invalid {kind} event: {error}"),
            })?;

        match kind {
            "message_start" => {
                self.model = value["message"]["model"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if let Ok(raw) =
                    serde_json::from_value::<RawUsage>(value["message"]["usage"].clone())
                {
                    self.usage = map_usage(&raw);
                    return Ok(Some(ModelEvent::UsageUpdate {
                        usage: self.usage.clone(),
                    }));
                }
            }
            "content_block_start" => {
                let index = value["index"].as_u64().unwrap_or_default() as usize;
                let content = &value["content_block"];
                if content["type"] == "tool_use" {
                    let id = ToolCallId::new();
                    if let Some(provider_id) = content["id"].as_str() {
                        self.tool_ids
                            .lock()
                            .expect("provider tool-id map poisoned")
                            .insert(id, provider_id.to_string());
                    }
                    let name = content["name"].as_str().unwrap_or_default().to_string();
                    let initial = content
                        .get("input")
                        .filter(|input| {
                            !input.is_null()
                                && input.as_object().map_or(true, |object| !object.is_empty())
                        })
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    self.tools.insert(
                        index,
                        ToolBuffer {
                            id: Some(id),
                            name: name.clone(),
                            json: initial,
                        },
                    );
                    return Ok(Some(ModelEvent::ToolCallStarted { id, name }));
                }
            }
            "content_block_delta" => {
                let index = value["index"].as_u64().unwrap_or_default() as usize;
                let delta = &value["delta"];
                return match delta["type"].as_str() {
                    Some("text_delta") => Ok(Some(ModelEvent::TextDelta {
                        delta: delta["text"].as_str().unwrap_or_default().to_string(),
                    })),
                    Some("thinking_delta") => Ok(Some(ModelEvent::ReasoningDelta {
                        delta: delta["thinking"].as_str().unwrap_or_default().to_string(),
                    })),
                    Some("input_json_delta") => {
                        if let Some(tool) = self.tools.get_mut(&index) {
                            let fragment = delta["partial_json"].as_str().unwrap_or_default();
                            tool.json.push_str(fragment);
                            Ok(Some(ModelEvent::ToolCallDelta {
                                id: tool.id.expect("tool id"),
                                delta: fragment.to_string(),
                            }))
                        } else {
                            Ok(None)
                        }
                    }
                    _ => Ok(None),
                };
            }
            "content_block_stop" => {
                let index = value["index"].as_u64().unwrap_or_default() as usize;
                if let Some(tool) = self.tools.remove(&index) {
                    let input = if tool.json.is_empty() {
                        serde_json::json!({})
                    } else {
                        serde_json::from_str(&tool.json).map_err(|error| ModelError::Protocol {
                            message: format!("invalid tool input: {error}"),
                        })?
                    };
                    return Ok(Some(ModelEvent::ToolCallCompleted {
                        id: tool.id.expect("tool id"),
                        name: tool.name,
                        input,
                    }));
                }
            }
            "message_delta" => {
                if let Some(reason) = value["delta"]["stop_reason"].as_str() {
                    self.stop_reason = reason.to_string();
                }
                if let Ok(raw) = serde_json::from_value::<RawUsage>(value["usage"].clone()) {
                    self.usage = merge_usage(&self.usage, &raw);
                    return Ok(Some(ModelEvent::UsageUpdate {
                        usage: self.usage.clone(),
                    }));
                }
            }
            "message_stop" => self.saw_message_stop = true,
            "error" => {
                return Err(ModelError::BackendError {
                    message: value["error"]["message"]
                        .as_str()
                        .unwrap_or("Anthropic stream error")
                        .to_string(),
                    code: value["error"]["type"]
                        .as_str()
                        .unwrap_or("stream_error")
                        .to_string(),
                });
            }
            "ping" => {}
            _ => {}
        }
        Ok(None)
    }
}

fn find_sse_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) if left <= right => Some((left, 2)),
        (Some(_), Some(right)) => Some((right, 4)),
        (Some(left), None) => Some((left, 2)),
        (None, Some(right)) => Some((right, 4)),
        (None, None) => None,
    }
}

fn map_usage(raw: &RawUsage) -> ModelUsage {
    AnthropicUsageMapper::map_usage(
        &RawAnthropicUsage {
            input_tokens: raw.input_tokens,
            output_tokens: raw.output_tokens,
            cache_read_input_tokens: raw.cache_read_input_tokens,
            cache_write_input_tokens: raw.cache_write_input_tokens,
        },
        "",
    )
}

fn merge_usage(previous: &ModelUsage, raw: &RawUsage) -> ModelUsage {
    let mapped = map_usage(raw);
    let input_tokens = if mapped.input_tokens.is_unknown() {
        previous.input_tokens
    } else {
        mapped.input_tokens
    };
    let output_tokens = if mapped.output_tokens.is_unknown() {
        previous.output_tokens
    } else {
        mapped.output_tokens
    };
    let total_tokens = input_tokens.checked_add(output_tokens);

    ModelUsage {
        input_tokens,
        output_tokens,
        cache_read_tokens: if mapped.cache_read_tokens.is_unknown() {
            previous.cache_read_tokens
        } else {
            mapped.cache_read_tokens
        },
        cache_write_tokens: if mapped.cache_write_tokens.is_unknown() {
            previous.cache_write_tokens
        } else {
            mapped.cache_write_tokens
        },
        reasoning_tokens: previous.reasoning_tokens,
        total_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::ids::{MessageId, Timestamp};
    use harness_protocol::messages::{ContentBlock, MessageRole};

    /// M4: an image content block must reach the wire as a real base64
    /// `image` block — not be silently dropped, which was the pre-M4
    /// behavior (`ContentBlock::Image { .. } => None`).
    #[test]
    fn image_content_block_becomes_a_real_anthropic_image_block() {
        let message = AgentMessage {
            id: MessageId::new(),
            role: MessageRole::User,
            content: vec![ContentBlock::Image {
                mime_type: "image/png".to_string(),
                data: vec![1, 2, 3, 4],
            }],
            created_at: Timestamp::now(),
        };

        let converted = agent_message_to_anthropic(&message);
        assert_eq!(
            converted.content.len(),
            1,
            "image block must not be dropped"
        );
        match &converted.content[0] {
            AnthropicContentBlock::Image { source } => {
                assert_eq!(source.kind, "base64");
                assert_eq!(source.media_type, "image/png");
                assert_eq!(
                    source.data,
                    base64::engine::general_purpose::STANDARD.encode([1, 2, 3, 4])
                );
            }
            other => panic!("expected an Image block, got {other:?}"),
        }
    }

    const FIXTURE: &str = r#"event: message_start
data: {"message":{"model":"claude-sonnet-4-20250513","usage":{"input_tokens":2,"output_tokens":0}}}

event: content_block_delta
data: {"index":0,"delta":{"type":"text_delta","text":"hi"}}

event: message_delta
data: {"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}

event: message_stop
data: {"type":"message_stop"}

"#;

    #[test]
    fn incremental_parser_handles_single_byte_chunks() {
        let mut parser = AnthropicSseParser::new();
        let mut events = Vec::new();
        for byte in FIXTURE.as_bytes() {
            events.extend(
                parser
                    .push_chunk(std::slice::from_ref(byte))
                    .expect("valid chunk"),
            );
        }
        let (terminal, result) = parser.finish().expect("complete fixture");
        events.extend(terminal);

        assert!(events
            .iter()
            .any(|event| { matches!(event, ModelEvent::TextDelta { delta } if delta == "hi") }));
        assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
        assert_eq!(result.usage.input_tokens.value(), Some(2));
        assert_eq!(result.usage.output_tokens.value(), Some(1));
        assert_eq!(result.usage.total_tokens.value(), Some(3));
    }

    #[test]
    fn parser_rejects_a_stream_without_message_stop() {
        let mut parser = AnthropicSseParser::new();
        parser
            .push_chunk(b"event: ping\ndata: {}\n\n")
            .expect("ping parses");
        assert!(matches!(parser.finish(), Err(ModelError::Protocol { .. })));
    }
}
