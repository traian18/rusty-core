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
    /// Set only when emulating structured output — see
    /// [`STRUCTURED_OUTPUT_TOOL`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<AnthropicToolChoice>,
    pub stream: bool,
}

/// Name of the synthetic tool used to emulate `response_format`.
///
/// The Messages API has no `response_format` field. The supported way to get
/// schema-conforming JSON is to declare a tool whose `input_schema` *is* the
/// desired schema and force `tool_choice` onto it: the model's tool-call
/// input is then guaranteed to match. The harness surfaces that input as
/// ordinary assistant text, so callers see the same thing they would from a
/// provider with a native `response_format`.
///
/// Prefixed to make a collision with a real host tool implausible.
pub const STRUCTURED_OUTPUT_TOOL: &str = "__harness_structured_response";

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicToolChoice {
    Auto,
    Any,
    Tool { name: String },
}

/// Build the synthetic tool and forced choice for a response format, or
/// `None` when the format needs no emulation.
pub fn structured_output_tool(
    format: &harness_protocol::backend::ResponseFormat,
) -> Option<(AnthropicTool, AnthropicToolChoice)> {
    use harness_protocol::backend::ResponseFormat;
    let (description, schema) = match format {
        ResponseFormat::Text => return None,
        ResponseFormat::JsonObject => (
            "Return your entire response as a single JSON object.".to_string(),
            // No constraint beyond "an object" — mirrors what OpenAI's
            // `json_object` mode guarantees.
            serde_json::json!({ "type": "object" }),
        ),
        ResponseFormat::JsonSchema { name, schema, .. } => (
            format!("Return your entire response as JSON matching the `{name}` schema."),
            schema.clone(),
        ),
    };
    Some((
        AnthropicTool {
            name: STRUCTURED_OUTPUT_TOOL.to_string(),
            description,
            input_schema: schema,
        },
        AnthropicToolChoice::Tool {
            name: STRUCTURED_OUTPUT_TOOL.to_string(),
        },
    ))
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

/// Resolves the outgoing `thinking` block from a [`ModelRequest`](harness_model::request::ModelRequest)'s
/// own `extended_thinking`/`reasoning_effort` fields, or `None` when neither
/// asks for it.
///
/// Both fields can request thinking independently -- `extended_thinking` is
/// a plain on/off flag (used when a caller wants thinking without picking a
/// level), `reasoning_effort` is a three-level enum (used when a caller
/// picked a level explicitly, e.g. from a model reference's own
/// `::reasoning=` suffix). An explicit level scales the budget; a bare
/// `extended_thinking: true` with no level keeps the original behavior of
/// using the full available budget.
pub fn resolve_thinking(
    extended_thinking: bool,
    reasoning_effort: Option<harness_protocol::backend::ReasoningEffort>,
    max_tokens: u64,
) -> Result<Option<AnthropicThinking>, ModelError> {
    if !extended_thinking && reasoning_effort.is_none() {
        return Ok(None);
    }
    if max_tokens < 2048 {
        return Err(ModelError::InvalidRequest {
            message: "extended thinking requires max_tokens >= 2048".to_string(),
        });
    }
    let full_budget = max_tokens - 1024;
    use harness_protocol::backend::ReasoningEffort;
    let budget_tokens = match reasoning_effort {
        Some(ReasoningEffort::Low) => (full_budget / 4).max(1024).min(full_budget),
        Some(ReasoningEffort::Medium) => (full_budget / 2).max(1024).min(full_budget),
        Some(ReasoningEffort::High) | None => full_budget,
    };
    Ok(Some(AnthropicThinking {
        kind: "enabled".to_string(),
        budget_tokens,
    }))
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

/// Anthropic's Messages API requires every tool `name` to match
/// `^[a-zA-Z0-9_-]{1,128}$`. Two of this workspace's own built-in tools
/// (`web.fetch`'s advertised name, `agent.spawn`) violated this at one
/// point and were fixed at the source; this check exists so any *other*
/// offender -- most plausibly a name reported verbatim by a third-party MCP
/// server, which this workspace does not control -- fails with a clear,
/// locally-raised error naming the exact tool, instead of an opaque
/// provider-side 400 the caller has to reverse-engineer an index out of.
pub fn is_valid_anthropic_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Validates every tool's name before a request is built, returning the
/// names of every offender (there may be more than one -- surfacing all of
/// them at once saves a caller from fixing one, retrying, and hitting the
/// next).
pub fn find_invalid_tool_names(tools: &[ToolDescriptor]) -> Vec<&str> {
    tools
        .iter()
        .map(|tool| tool.name.as_str())
        .filter(|name| !is_valid_anthropic_tool_name(name))
        .collect()
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
                    let events = self.parse_non_streaming_json(&value)?;
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

        if event_type == Some("message_stop") {
            self.saw_message_stop = true;
            return Ok(None);
        }

        let trimmed_data = data.trim();
        if trimmed_data == "[DONE]" {
            self.saw_message_stop = true;
            return Ok(None);
        }

        if trimmed_data.is_empty() {
            return Ok(None);
        }

        let value: serde_json::Value =
            serde_json::from_str(&data).map_err(|error| ModelError::Protocol {
                message: format!("invalid event data: {error}"),
            })?;

        let kind = event_type
            .or_else(|| value.get("type").and_then(|t| t.as_str()))
            .unwrap_or("");

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
                    // The structured-output tool is an implementation detail
                    // of `response_format`; it is never registered with the
                    // host and must not surface as a tool call.
                    if content["name"] == STRUCTURED_OUTPUT_TOOL {
                        self.structured_output_blocks.insert(index);
                        let initial = content
                            .get("input")
                            .filter(|input| {
                                !input.is_null()
                                    && input.as_object().map_or(true, |object| !object.is_empty())
                            })
                            .map(ToString::to_string);
                        return Ok(initial.map(|delta| ModelEvent::TextDelta { delta }));
                    }
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
                    Some("input_json_delta") if self.structured_output_blocks.contains(&index) => {
                        Ok(Some(ModelEvent::TextDelta {
                            delta: delta["partial_json"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                        }))
                    }
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
                if self.structured_output_blocks.remove(&index) {
                    // Already fully emitted as text deltas; there is no tool
                    // call to complete.
                    return Ok(None);
                }
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

    fn parse_non_streaming_json(
        &mut self,
        value: &serde_json::Value,
    ) -> Result<Vec<ModelEvent>, ModelError> {
        let mut events = Vec::new();
        if let Some(model) = value.get("model").and_then(|m| m.as_str()) {
            self.model = model.to_string();
        }
        if let Some(reason) = value.get("stop_reason").and_then(|r| r.as_str()) {
            self.stop_reason = reason.to_string();
        }
        if let Ok(raw) = serde_json::from_value::<RawUsage>(value["usage"].clone()) {
            self.usage = map_usage(&raw);
            events.push(ModelEvent::UsageUpdate {
                usage: self.usage.clone(),
            });
        }

        if let Some(content_blocks) = value.get("content").and_then(|c| c.as_array()) {
            for block in content_blocks {
                let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match block_type {
                    "text" => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            events.push(ModelEvent::TextDelta {
                                delta: text.to_string(),
                            });
                        }
                    }
                    "thinking" => {
                        if let Some(thinking) = block.get("thinking").and_then(|t| t.as_str()) {
                            events.push(ModelEvent::ReasoningDelta {
                                delta: thinking.to_string(),
                            });
                        }
                    }
                    "tool_use" => {
                        let name = block.get("name").and_then(|n| n.as_str()).unwrap_or_default();
                        let input = block.get("input").cloned().unwrap_or(serde_json::json!({}));
                        if name == STRUCTURED_OUTPUT_TOOL {
                            events.push(ModelEvent::TextDelta {
                                delta: input.to_string(),
                            });
                        } else {
                            let id = ToolCallId::new();
                            if let Some(provider_id) = block.get("id").and_then(|i| i.as_str()) {
                                self.tool_ids
                                    .lock()
                                    .expect("provider tool-id map poisoned")
                                    .insert(id, provider_id.to_string());
                            }
                            events.push(ModelEvent::ToolCallStarted {
                                id,
                                name: name.to_string(),
                            });
                            events.push(ModelEvent::ToolCallCompleted {
                                id,
                                name: name.to_string(),
                                input,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(events)
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
    use harness_protocol::backend::{ReasoningEffort, ResponseFormat};
    use harness_protocol::ids::{MessageId, Timestamp};
    use harness_protocol::messages::{ContentBlock, MessageRole};

    #[test]
    fn valid_tool_names_pass() {
        assert!(is_valid_anthropic_tool_name("web_fetch"));
        assert!(is_valid_anthropic_tool_name("agent_spawn"));
        assert!(is_valid_anthropic_tool_name("get-weather"));
        assert!(is_valid_anthropic_tool_name("ABC123_-"));
    }

    #[test]
    fn invalid_tool_names_are_rejected() {
        assert!(!is_valid_anthropic_tool_name(""), "empty name");
        assert!(!is_valid_anthropic_tool_name("web.fetch"), "dot");
        assert!(!is_valid_anthropic_tool_name("Fetch URL"), "space");
        assert!(!is_valid_anthropic_tool_name("mcp:get_weather"), "colon");
        assert!(!is_valid_anthropic_tool_name(&"x".repeat(129)), "over 128 chars");
    }

    #[test]
    fn find_invalid_tool_names_reports_every_offender_not_just_the_first() {
        let tools = vec![
            ToolDescriptor {
                id: harness_protocol::ids::ToolId::new(),
                name: "read_file".to_string(),
                description: String::new(),
                input_schema: serde_json::json!({}),
            },
            ToolDescriptor {
                id: harness_protocol::ids::ToolId::new(),
                name: "mcp.weather.get-forecast".to_string(),
                description: String::new(),
                input_schema: serde_json::json!({}),
            },
            ToolDescriptor {
                id: harness_protocol::ids::ToolId::new(),
                name: "Fetch URL".to_string(),
                description: String::new(),
                input_schema: serde_json::json!({}),
            },
        ];
        let invalid = find_invalid_tool_names(&tools);
        assert_eq!(invalid, vec!["mcp.weather.get-forecast", "Fetch URL"]);
    }

    #[test]
    fn no_thinking_requested_produces_no_thinking_block() {
        assert!(resolve_thinking(false, None, 8192).unwrap().is_none());
    }

    #[test]
    fn bare_extended_thinking_uses_the_full_available_budget() {
        let thinking = resolve_thinking(true, None, 8192).unwrap().unwrap();
        assert_eq!(thinking.kind, "enabled");
        assert_eq!(thinking.budget_tokens, 8192 - 1024);
    }

    #[test]
    fn an_explicit_reasoning_effort_scales_the_budget() {
        let full = 8192 - 1024;
        let low = resolve_thinking(false, Some(ReasoningEffort::Low), 8192).unwrap().unwrap();
        let medium = resolve_thinking(false, Some(ReasoningEffort::Medium), 8192).unwrap().unwrap();
        let high = resolve_thinking(false, Some(ReasoningEffort::High), 8192).unwrap().unwrap();
        assert_eq!(low.budget_tokens, full / 4);
        assert_eq!(medium.budget_tokens, full / 2);
        assert_eq!(high.budget_tokens, full);
        assert!(low.budget_tokens < medium.budget_tokens);
        assert!(medium.budget_tokens < high.budget_tokens);
    }

    #[test]
    fn reasoning_effort_alone_requests_thinking_even_without_the_extended_thinking_flag() {
        // Regression test: previously `reasoning_effort` was never read by
        // this crate at all, so a caller that only set `reasoning_effort`
        // (e.g. from a model reference's `::reasoning=` suffix, never
        // `extended_thinking`) got silently no thinking block, even though
        // the capability check let the request through.
        assert!(resolve_thinking(false, Some(ReasoningEffort::Low), 8192).unwrap().is_some());
    }

    #[test]
    fn low_effort_budget_never_drops_below_the_1024_floor_on_a_small_max_tokens() {
        // full_budget = 2048 - 1024 = 1024; full_budget / 4 = 256, which the
        // `.max(1024)` floor must bring back up to 1024, clamped to not
        // exceed full_budget by the trailing `.min(full_budget)`.
        let low = resolve_thinking(false, Some(ReasoningEffort::Low), 2048).unwrap().unwrap();
        assert_eq!(low.budget_tokens, 1024);
    }

    #[test]
    fn thinking_requires_max_tokens_at_least_2048() {
        assert!(matches!(
            resolve_thinking(true, None, 2047),
            Err(ModelError::InvalidRequest { .. })
        ));
        assert!(matches!(
            resolve_thinking(false, Some(ReasoningEffort::Low), 1024),
            Err(ModelError::InvalidRequest { .. })
        ));
    }

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
        let error = parser.finish().expect_err("must reject a stream missing message_stop");
        assert!(matches!(error, ModelError::StreamInterrupted { .. }));
        assert!(error.is_retryable(), "a dropped connection, not malformed data, should be retried");
    }

    #[test]
    fn parser_accepts_message_stop_with_empty_data() {
        let mut parser = AnthropicSseParser::new();
        parser
            .push_chunk(b"event: message_stop\n\n")
            .expect("message_stop parses");
        let (events, _) = parser.finish().expect("must accept empty message_stop");
        assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
    }

    #[test]
    fn parser_accepts_data_done_sentinel() {
        let mut parser = AnthropicSseParser::new();
        parser
            .push_chunk(b"data: [DONE]\n\n")
            .expect("[DONE] parses");
        let (events, _) = parser.finish().expect("must accept [DONE]");
        assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
    }

    #[test]
    fn parser_accepts_data_only_message_stop() {
        let mut parser = AnthropicSseParser::new();
        parser
            .push_chunk(b"data: {\"type\":\"message_stop\"}\n\n")
            .expect("data-only message_stop parses");
        let (events, _) = parser.finish().expect("must accept data-only message_stop");
        assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
    }

    #[test]
    fn parser_handles_non_streaming_json_message_fallback() {
        let json_body = serde_json::json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [
                { "type": "text", "text": "Hello from OpenCode Haiku" }
            ],
            "model": "claude-3-5-haiku-20241022",
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 12,
                "output_tokens": 6
            }
        });
        let mut parser = AnthropicSseParser::new();
        parser
            .push_chunk(json_body.to_string().as_bytes())
            .expect("json body chunked");
        let (events, result) = parser.finish().expect("finish parses non-streaming json");
        assert!(events.iter().any(|e| matches!(e, ModelEvent::TextDelta { delta } if delta == "Hello from OpenCode Haiku")));
        assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
        assert_eq!(result.usage.input_tokens.value(), Some(12));
        assert_eq!(result.usage.output_tokens.value(), Some(6));
    }

    // -----------------------------------------------------------------
    // Structured output (emulated — Anthropic has no `response_format`)
    // -----------------------------------------------------------------

    #[test]
    fn json_schema_becomes_a_forced_single_purpose_tool() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "title": { "type": "string" } },
            "required": ["title"],
        });
        let (tool, choice) = structured_output_tool(&ResponseFormat::JsonSchema {
            name: "task_graph".into(),
            schema: schema.clone(),
            strict: true,
        })
        .expect("a schema format must produce a tool");

        assert_eq!(tool.name, STRUCTURED_OUTPUT_TOOL);
        // The caller's schema is the tool's input schema verbatim — that is
        // the whole mechanism by which the response conforms.
        assert_eq!(tool.input_schema, schema);
        assert!(
            tool.description.contains("task_graph"),
            "the schema name should reach the model as a hint: {}",
            tool.description
        );
        assert_eq!(
            choice,
            AnthropicToolChoice::Tool {
                name: STRUCTURED_OUTPUT_TOOL.into()
            },
            "the tool must be forced, not merely offered"
        );
    }

    #[test]
    fn text_format_needs_no_emulation() {
        assert!(structured_output_tool(&ResponseFormat::Text).is_none());
    }

    #[test]
    fn json_object_constrains_only_to_an_object() {
        let (tool, _) =
            structured_output_tool(&ResponseFormat::JsonObject).expect("must produce a tool");
        assert_eq!(tool.input_schema, serde_json::json!({ "type": "object" }));
    }

    /// The important one: the synthetic tool's input must reach the caller as
    /// assistant **text**. If it leaked through as a tool call, the runtime
    /// would try to execute a tool that was never registered, and a caller
    /// that asked for JSON would receive empty text.
    #[test]
    fn structured_output_tool_input_streams_back_as_text_not_a_tool_call() {
        let mut parser = AnthropicSseParser::new();
        let mut events = Vec::new();

        for chunk in [
            format!(
                "event: content_block_start\ndata: {{\"index\":0,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"{STRUCTURED_OUTPUT_TOOL}\",\"input\":{{}}}}}}\n\n"
            ),
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"title\\\":\"}}\n\n".to_string(),
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"hi\\\"}\"}}\n\n".to_string(),
            "event: content_block_stop\ndata: {\"index\":0}\n\n".to_string(),
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n".to_string(),
            "event: message_stop\ndata: {}\n\n".to_string(),
        ] {
            events.extend(parser.push_chunk(chunk.as_bytes()).expect("chunk parses"));
        }

        let text: String = events
            .iter()
            .filter_map(|event| match event {
                ModelEvent::TextDelta { delta } => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            text, r#"{"title":"hi"}"#,
            "the tool input should reassemble as the response text"
        );

        assert!(
            !events.iter().any(|event| matches!(
                event,
                ModelEvent::ToolCallStarted { .. }
                    | ModelEvent::ToolCallDelta { .. }
                    | ModelEvent::ToolCallCompleted { .. }
            )),
            "the synthetic tool must not surface as a tool call: {events:?}"
        );
    }

    /// A *real* tool call in the same stream must keep behaving normally —
    /// the suppression is keyed on the synthetic tool's name, not on
    /// "any tool call while a response format is set".
    #[test]
    fn a_real_tool_call_is_unaffected_by_the_structured_output_path() {
        let mut parser = AnthropicSseParser::new();
        let mut events = Vec::new();

        for chunk in [
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"fs.read\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        ] {
            events.extend(parser.push_chunk(chunk.as_bytes()).expect("chunk parses"));
        }

        assert!(
            events
                .iter()
                .any(|event| matches!(event, ModelEvent::ToolCallStarted { name, .. } if name == "fs.read")),
            "a real tool call must still start: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ModelEvent::ToolCallCompleted { .. })),
            "a real tool call must still complete: {events:?}"
        );
    }
}
