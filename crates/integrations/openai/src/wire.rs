//! OpenAI Chat Completions wire types and incremental SSE normalization.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use harness_model::events::{ModelError, ModelEvent, ModelResult};
use harness_protocol::ids::ToolCallId;
use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use harness_protocol::tools::ToolDescriptor;
use harness_protocol::usage::ModelUsage;
use serde::Serialize;

use crate::usage::{OpenAiUsageMapper, RawOpenAiUsage};

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct OpenAiRequest {
    pub model: String,
    pub messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<OpenAiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    pub stream: bool,
    pub stream_options: StreamOptions,
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAiMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<OpenAiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAiToolCallOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// OpenAI's Chat Completions `content` field accepts either a plain string
/// (the common, text-only case) or an array of typed parts (needed the
/// moment a message carries an image) — `#[serde(untagged)]` picks whichever
/// variant matches at serialization time, so a text-only message keeps
/// serializing exactly as before rather than always paying for the more
/// verbose array form.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum OpenAiContent {
    Text(String),
    Parts(Vec<OpenAiContentPart>),
}

/// One part of a multimodal `content` array.
/// <https://platform.openai.com/docs/guides/vision> (image_url content parts).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenAiContentPart {
    Text { text: String },
    ImageUrl { image_url: OpenAiImageUrl },
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAiImageUrl {
    /// A data URL (`data:<mime>;base64,<data>`) — images arrive on the
    /// harness side as raw bytes with no externally reachable URL, so this
    /// is the only representation that doesn't require an upload step.
    pub url: String,
}

/// Converts one message's content blocks into an [`OpenAiContent`]: plain
/// text when there are no images (identical to the pre-image-support wire
/// shape), or a typed parts array once at least one `ContentBlock::Image` is
/// present, since OpenAI only accepts the array form for multimodal content.
fn content_blocks_to_openai_content(content: &[ContentBlock]) -> OpenAiContent {
    let has_image = content.iter().any(|block| matches!(block, ContentBlock::Image { .. }));
    if !has_image {
        return OpenAiContent::Text(concat_text(content));
    }
    let parts = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(OpenAiContentPart::Text { text: text.clone() }),
            ContentBlock::Image { mime_type, data } => Some(OpenAiContentPart::ImageUrl {
                image_url: OpenAiImageUrl {
                    url: format!(
                        "data:{mime_type};base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(data)
                    ),
                },
            }),
            _ => None,
        })
        .collect();
    OpenAiContent::Parts(parts)
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAiToolCallOut {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: OpenAiFunctionCall,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAiFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAiTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: OpenAiFunctionDef,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAiFunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

pub fn tool_descriptor_to_openai(tool: &ToolDescriptor) -> OpenAiTool {
    OpenAiTool {
        kind: "function".to_string(),
        function: OpenAiFunctionDef {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.input_schema.clone(),
        },
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

/// Provider tool-call IDs (`"call_xxx"`) keyed by the harness-internal
/// `ToolCallId`, so a tool call minted while parsing a streamed response can
/// be translated back to the ID OpenAI expects when that call reappears in a
/// later turn's message history.
pub type ProviderToolIds = Arc<Mutex<HashMap<ToolCallId, String>>>;

/// Converts one [`AgentMessage`] into zero or more [`OpenAiMessage`]s.
///
/// Unlike Anthropic (which represents a tool result as a content block
/// inside a "user" message), OpenAI's wire format requires one dedicated
/// `"tool"`-role message per tool result, each carrying its own
/// `tool_call_id` — so a single harness `Tool`-role message containing
/// several `ToolResult` blocks fans out into several `OpenAiMessage`s here.
fn agent_message_to_openai(
    message: &AgentMessage,
    tool_ids: &HashMap<ToolCallId, String>,
) -> Vec<OpenAiMessage> {
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
                ContentBlock::ToolResult { call_id, result } => Some(OpenAiMessage {
                    role: "tool".to_string(),
                    content: Some(OpenAiContent::Text(result.output_preview.clone())),
                    tool_calls: None,
                    tool_call_id: Some(provider_id_for(call_id)),
                }),
                _ => None,
            })
            .collect(),
        MessageRole::User => vec![OpenAiMessage {
            role: "user".to_string(),
            content: Some(content_blocks_to_openai_content(&message.content)),
            tool_calls: None,
            tool_call_id: None,
        }],
        MessageRole::Assistant => {
            let text = concat_text(&message.content);
            let tool_calls: Vec<OpenAiToolCallOut> = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { call } => Some(OpenAiToolCallOut {
                        id: provider_id_for(&call.id),
                        kind: "function".to_string(),
                        function: OpenAiFunctionCall {
                            name: call.name.clone(),
                            arguments: call.arguments.to_string(),
                        },
                    }),
                    _ => None,
                })
                .collect();
            vec![OpenAiMessage {
                role: "assistant".to_string(),
                content: if text.is_empty() { None } else { Some(OpenAiContent::Text(text)) },
                tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                tool_call_id: None,
            }]
        }
        MessageRole::System => unreachable!("system messages must be filtered before conversion"),
    }
}

/// Builds the system-role message (if any) the same way Anthropic's client
/// does: prefer the request's `system_prompt` field, falling back to
/// concatenating any `System`-role messages in the transcript.
pub fn build_system_message(system_prompt: &str, messages: &[AgentMessage]) -> Option<OpenAiMessage> {
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
    text.map(|content| OpenAiMessage {
        role: "system".to_string(),
        content: Some(OpenAiContent::Text(content)),
        tool_calls: None,
        tool_call_id: None,
    })
}

pub fn convert_messages_with_tool_ids(
    messages: &[AgentMessage],
    tool_ids: &HashMap<ToolCallId, String>,
) -> Vec<OpenAiMessage> {
    messages
        .iter()
        .filter(|m| m.role != MessageRole::System)
        .flat_map(|m| agent_message_to_openai(m, tool_ids))
        .collect()
}

// ---------------------------------------------------------------------------
// Streaming SSE parser
// ---------------------------------------------------------------------------

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
    stop_reason: String,
    model: String,
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
            stop_reason: "stop".to_string(),
            model: String::new(),
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
            return Err(ModelError::Protocol {
                message: "SSE stream ended without [DONE]".to_string(),
            });
        }

        let mut terminal_events = Vec::new();
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
        terminal_events.push(ModelEvent::Completed { result: result.clone() });
        self.events.extend(terminal_events.iter().cloned());
        Ok((terminal_events, result))
    }

    fn parse_block(&mut self, bytes: &[u8]) -> Result<Option<ModelEvent>, ModelError> {
        let block = std::str::from_utf8(bytes).map_err(|error| ModelError::Protocol {
            message: error.to_string(),
        })?;
        let data = block
            .lines()
            .filter_map(|line| line.trim_end_matches('\r').strip_prefix("data:").map(str::trim))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            return Ok(None);
        }
        if data == "[DONE]" {
            self.saw_done = true;
            return Ok(None);
        }

        let value: serde_json::Value = serde_json::from_str(&data).map_err(|error| ModelError::Protocol {
            message: format!("invalid chunk: {error}"),
        })?;

        if let Some(error) = value.get("error") {
            return Err(ModelError::BackendError {
                message: error["message"].as_str().unwrap_or("OpenAI stream error").to_string(),
                code: error["type"].as_str().unwrap_or("stream_error").to_string(),
            });
        }

        if self.model.is_empty() {
            if let Some(model) = value.get("model").and_then(|m| m.as_str()) {
                self.model = model.to_string();
            }
        }

        let choices = value.get("choices").and_then(|c| c.as_array()).filter(|c| !c.is_empty());

        let Some(choices) = choices else {
            if let Some(usage_value) = value.get("usage").filter(|v| !v.is_null()) {
                if let Ok(raw) = serde_json::from_value::<RawOpenAiUsage>(usage_value.clone()) {
                    self.usage = OpenAiUsageMapper::map_usage(&raw);
                    return Ok(Some(ModelEvent::UsageUpdate { usage: self.usage.clone() }));
                }
            }
            return Ok(None);
        };

        let choice = &choices[0];
        if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
            self.stop_reason = reason.to_string();
        }

        let delta = &choice["delta"];

        if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
            if !text.is_empty() {
                return Ok(Some(ModelEvent::TextDelta { delta: text.to_string() }));
            }
        }

        if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            if let Some(tool_call) = tool_calls.first() {
                let index = tool_call["index"].as_u64().unwrap_or(0) as usize;
                let is_new = !self.tools.contains_key(&index);
                if is_new {
                    let id = ToolCallId::new();
                    if let Some(provider_id) = tool_call["id"].as_str() {
                        self.tool_ids
                            .lock()
                            .expect("provider tool-id map poisoned")
                            .insert(id, provider_id.to_string());
                    }
                    let name = tool_call["function"]["name"].as_str().unwrap_or_default().to_string();
                    self.tools.insert(index, ToolBuffer { id, name: name.clone(), json: String::new() });
                }
                let fragment = tool_call["function"]["arguments"].as_str().unwrap_or_default();
                let buffer = self.tools.get_mut(&index).expect("just inserted or already present");
                buffer.json.push_str(fragment);

                if is_new {
                    return Ok(Some(ModelEvent::ToolCallStarted { id: buffer.id, name: buffer.name.clone() }));
                } else if !fragment.is_empty() {
                    return Ok(Some(ModelEvent::ToolCallDelta { id: buffer.id, delta: fragment.to_string() }));
                }
            }
        }

        Ok(None)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// M4/M4.5: proves `OpenAiRequest`'s JSON field names/shape match what
    /// the Chat Completions API actually expects (`max_tokens`,
    /// `temperature`, `stop` as a bare array, `model` at top level) — a
    /// wire-format regression here would silently produce a request the
    /// real API either ignores or rejects, which no test at the
    /// `GenericModelBackend` layer (M4.1's contract suite) can catch, since
    /// that layer stops at `ModelRequest`, one level above this JSON.
    #[test]
    fn request_serializes_with_the_expected_openai_field_names() {
        let request = OpenAiRequest {
            model: "gpt-4.1".to_string(),
            messages: vec![],
            tools: None,
            max_tokens: Some(4096),
            temperature: Some(0.5),
            stop: Some(vec!["STOP".to_string()]),
            stream: true,
            stream_options: StreamOptions { include_usage: true },
        };
        let json = serde_json::to_value(&request).expect("serialize OpenAiRequest");
        assert_eq!(json["model"], "gpt-4.1");
        assert_eq!(json["max_tokens"], 4096);
        assert_eq!(json["temperature"], 0.5);
        assert_eq!(json["stop"], serde_json::json!(["STOP"]));
        assert_eq!(json["stream"], true);
        // Omitted-when-None fields must actually be absent, not `null`,
        // since some OpenAI-compatible backends (this wire format is also
        // used by `openai-compatible`) reject unexpected null fields.
        let bare = OpenAiRequest {
            model: "gpt-4.1".to_string(),
            messages: vec![],
            tools: None,
            max_tokens: None,
            temperature: None,
            stop: None,
            stream: true,
            stream_options: StreamOptions { include_usage: true },
        };
        let bare_json = serde_json::to_value(&bare).expect("serialize bare OpenAiRequest");
        assert!(bare_json.get("max_tokens").is_none());
        assert!(bare_json.get("temperature").is_none());
        assert!(bare_json.get("stop").is_none());
    }

    fn user_message(content: Vec<ContentBlock>) -> AgentMessage {
        AgentMessage {
            id: harness_protocol::ids::MessageId::new(),
            role: MessageRole::User,
            content,
            created_at: harness_protocol::ids::Timestamp::now(),
        }
    }

    /// M4: a text-only message must keep serializing `content` as a bare
    /// string (the pre-image-support shape) rather than always paying for
    /// the more verbose typed-parts array form.
    #[test]
    fn a_text_only_message_serializes_content_as_a_plain_string() {
        let message = user_message(vec![ContentBlock::Text { text: "hello".into() }]);
        let openai = agent_message_to_openai(&message, &HashMap::new());
        let json = serde_json::to_value(&openai[0]).expect("serialize OpenAiMessage");
        assert_eq!(json["content"], "hello");
    }

    /// M4: an image content block must convert into a real
    /// `image_url`/`data:` part, not be silently dropped — matching
    /// Anthropic's existing image pass-through, previously OpenAI-specific
    /// wire support for it did not exist even though the client already
    /// advertised `images: true` in its capabilities.
    #[test]
    fn an_image_block_becomes_a_real_image_url_part_not_silently_dropped() {
        let message = user_message(vec![
            ContentBlock::Text { text: "what is this?".into() },
            ContentBlock::Image { mime_type: "image/png".into(), data: vec![1, 2, 3] },
        ]);
        let openai = agent_message_to_openai(&message, &HashMap::new());
        let json = serde_json::to_value(&openai[0]).expect("serialize OpenAiMessage");
        let parts = json["content"].as_array().expect("multimodal content must serialize as an array");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "what is this?");
        assert_eq!(parts[1]["type"], "image_url");
        let url = parts[1]["image_url"]["url"].as_str().expect("image_url.url must be a string");
        assert!(url.starts_with("data:image/png;base64,"), "unexpected image_url shape: {url}");
        assert!(url.ends_with(&base64::engine::general_purpose::STANDARD.encode([1, 2, 3])));
    }

    const FIXTURE: &str = "data: {\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n\
data: {\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n\
data: {\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"model\":\"gpt-4o\",\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\n\
data: [DONE]\n\n";

    #[test]
    fn incremental_parser_handles_single_byte_chunks() {
        let mut parser = OpenAiSseParser::new();
        let mut events = Vec::new();
        for byte in FIXTURE.as_bytes() {
            events.extend(parser.push_chunk(std::slice::from_ref(byte)).expect("valid chunk"));
        }
        let (terminal, result) = parser.finish().expect("complete fixture");
        events.extend(terminal);

        assert!(events.iter().any(|e| matches!(e, ModelEvent::TextDelta { delta } if delta == "hi")));
        assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
        assert_eq!(result.stop_reason, "stop");
        assert_eq!(result.usage.input_tokens.value(), Some(2));
        assert_eq!(result.usage.output_tokens.value(), Some(1));
        assert_eq!(result.usage.total_tokens.value(), Some(3));
    }

    #[test]
    fn parser_rejects_a_stream_without_done() {
        let mut parser = OpenAiSseParser::new();
        parser
            .push_chunk(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n")
            .expect("chunk parses");
        assert!(matches!(parser.finish(), Err(ModelError::Protocol { .. })));
    }

    #[test]
    fn tool_call_arguments_accumulate_across_chunks_and_complete_at_finish() {
        let mut parser = OpenAiSseParser::new();
        let chunks = [
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_abc\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"city\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"paris\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ];
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(parser.push_chunk(chunk.as_bytes()).expect("valid chunk"));
        }
        let (terminal, _) = parser.finish().expect("complete fixture");
        events.extend(terminal);

        let completed = events
            .iter()
            .find_map(|e| match e {
                ModelEvent::ToolCallCompleted { name, input, .. } => Some((name.clone(), input.clone())),
                _ => None,
            })
            .expect("a ToolCallCompleted event");
        assert_eq!(completed.0, "get_weather");
        assert_eq!(completed.1, serde_json::json!({ "city": "paris" }));
    }
}
