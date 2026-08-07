//! Gemini request wire types and incremental SSE normalization.
//!
//! Gemini's wire shape is meaningfully different from Anthropic/OpenAI, not
//! just a field-naming variation: messages are `contents: [{ role, parts }]`
//! where `parts` is a mixed array of text/functionCall/functionResponse
//! entries, and — notably — Gemini's function-calling protocol has **no
//! provider-issued call ID** at all: a `functionResponse` is matched back to
//! its `functionCall` purely by `name`. That's simpler than Anthropic/OpenAI
//! (no `ProviderToolIds` map, no persistent client-side state needed), but it
//! does mean this module has to reconstruct "which name does this
//! `ToolResult` belong to" by scanning the transcript for the matching
//! `ToolUse` block, since harness's own `ContentBlock::ToolResult` only
//! carries a `call_id`, not a name.

use std::collections::HashMap;

use base64::Engine;
use harness_model::events::{ModelError, ModelEvent, ModelResult};
use harness_protocol::ids::ToolCallId;
use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use harness_protocol::tools::ToolDescriptor;
use harness_protocol::usage::ModelUsage;
use serde::Serialize;

use crate::usage::{GeminiUsageMapper, RawGeminiUsage};

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiRequest {
    pub contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<GeminiSystemInstruction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<GeminiTool>>,
    pub generation_config: GeminiGenerationConfig,
}

#[derive(Debug, Clone, Serialize)]
pub struct GeminiContent {
    pub role: String,
    pub parts: Vec<GeminiPart>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_call: Option<GeminiFunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_response: Option<GeminiFunctionResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<GeminiInlineData>,
}

/// Gemini's inline (non-uploaded) media representation:
/// `{"inlineData":{"mimeType":"image/png","data":"<base64>"}}` — plain
/// base64, unlike OpenAI/Anthropic which each want a full data URL or a
/// dedicated `source` object.
/// <https://ai.google.dev/api/generate-content#blob> (inline media parts).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiInlineData {
    pub mime_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GeminiFunctionCall {
    pub name: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct GeminiFunctionResponse {
    pub name: String,
    pub response: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct GeminiSystemInstruction {
    pub parts: Vec<GeminiPart>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiTool {
    pub function_declarations: Vec<GeminiFunctionDeclaration>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GeminiFunctionDeclaration {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
}

pub fn tool_descriptor_to_gemini(tool: &ToolDescriptor) -> GeminiFunctionDeclaration {
    GeminiFunctionDeclaration {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.input_schema.clone(),
    }
}

fn text_part(text: String) -> GeminiPart {
    GeminiPart {
        text: Some(text),
        function_call: None,
        function_response: None,
        inline_data: None,
    }
}

fn image_part(mime_type: String, data: &[u8]) -> GeminiPart {
    GeminiPart {
        text: None,
        function_call: None,
        function_response: None,
        inline_data: Some(GeminiInlineData {
            mime_type,
            data: base64::engine::general_purpose::STANDARD.encode(data),
        }),
    }
}

/// Builds a `call_id -> name` lookup by scanning every `ToolUse` block in
/// the transcript — the only source of a tool's name once it's time to
/// convert a later `ToolResult` block (see module docs for why Gemini needs
/// this instead of a persistent provider-ID map).
fn build_call_names(messages: &[AgentMessage]) -> HashMap<ToolCallId, String> {
    let mut names = HashMap::new();
    for message in messages {
        for block in &message.content {
            if let ContentBlock::ToolUse { call } = block {
                names.insert(call.id, call.name.clone());
            }
        }
    }
    names
}

fn agent_message_to_gemini(
    message: &AgentMessage,
    call_names: &HashMap<ToolCallId, String>,
) -> Option<GeminiContent> {
    let parts: Vec<GeminiPart> = match message.role {
        MessageRole::User => message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text_part(text.clone())),
                ContentBlock::Image { mime_type, data } => Some(image_part(mime_type.clone(), data)),
                _ => None,
            })
            .collect(),
        MessageRole::Assistant => message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text_part(text.clone())),
                ContentBlock::ToolUse { call } => Some(GeminiPart {
                    text: None,
                    function_call: Some(GeminiFunctionCall {
                        name: call.name.clone(),
                        args: call.arguments.clone(),
                    }),
                    function_response: None,
                    inline_data: None,
                }),
                _ => None,
            })
            .collect(),
        // Gemini has no dedicated "tool" role — a function result is sent
        // back as a "user" turn containing a functionResponse part.
        MessageRole::Tool => message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult { call_id, result } => {
                    let name = call_names.get(call_id).cloned().unwrap_or_else(|| call_id.to_string());
                    Some(GeminiPart {
                        text: None,
                        function_call: None,
                        function_response: Some(GeminiFunctionResponse {
                            name,
                            response: serde_json::json!({ "result": result.output_preview }),
                        }),
                        inline_data: None,
                    })
                }
                _ => None,
            })
            .collect(),
        MessageRole::System => return None,
    };

    if parts.is_empty() {
        return None;
    }
    let role = match message.role {
        MessageRole::Assistant => "model",
        _ => "user",
    };
    Some(GeminiContent { role: role.to_string(), parts })
}

pub fn convert_messages(messages: &[AgentMessage]) -> Vec<GeminiContent> {
    let call_names = build_call_names(messages);
    messages
        .iter()
        .filter_map(|message| agent_message_to_gemini(message, &call_names))
        .collect()
}

/// Builds the `systemInstruction` field the same way the other providers
/// build a system prompt: prefer `system_prompt`, falling back to any
/// `System`-role messages in the transcript.
pub fn build_system_instruction(
    system_prompt: &str,
    messages: &[AgentMessage],
) -> Option<GeminiSystemInstruction> {
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
    text.map(|content| GeminiSystemInstruction {
        parts: vec![text_part(content)],
    })
}

// ---------------------------------------------------------------------------
// Streaming SSE parser
// ---------------------------------------------------------------------------

/// Stateful Gemini SSE parser.
///
/// Gemini's stream is plain `data: {...}\n\n` frames — no named `event:`
/// lines, and (unlike OpenAI) no `[DONE]` sentinel; the stream simply ends
/// when the last candidate's `finishReason` chunk has been sent and the
/// connection closes. Because a single chunk's `parts` array can contain
/// several parts at once (e.g. text followed by a function call), parsing
/// returns a `Vec<ModelEvent>` per block rather than at most one — the one
/// place this parser's shape has to differ from Anthropic's/OpenAI's.
pub struct GeminiSseParser {
    buffer: Vec<u8>,
    model: String,
    usage: ModelUsage,
    stop_reason: String,
    saw_finish_reason: bool,
    result: Option<ModelResult>,
    events: Vec<ModelEvent>,
}

impl GeminiSseParser {
    pub fn new(model: String) -> Self {
        Self {
            buffer: Vec::new(),
            model,
            usage: ModelUsage::default(),
            stop_reason: "STOP".to_string(),
            saw_finish_reason: false,
            result: None,
            events: Vec::new(),
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
        if self.result.is_some() {
            return Err(ModelError::Protocol {
                message: "SSE parser was finished more than once".to_string(),
            });
        }

        // Some SSE servers (and proxies) omit the final blank-line terminator
        // on the very last event before closing the connection — the last
        // block may still be sitting in `buffer` un-terminated. Parse it and
        // forward its events rather than silently dropping the final delta.
        let mut terminal_events = if !self.buffer.iter().all(u8::is_ascii_whitespace) {
            let trailing = std::mem::take(&mut self.buffer);
            self.parse_block(&trailing)?
        } else {
            Vec::new()
        };

        if !self.saw_finish_reason {
            return Err(ModelError::Protocol {
                message: "SSE stream ended without a finishReason".to_string(),
            });
        }

        let result = ModelResult {
            stop_reason: self.stop_reason.clone(),
            usage: self.usage.clone(),
            cost: GeminiUsageMapper::calculate_cost(&self.usage, &self.model),
        };
        self.result = Some(result.clone());
        terminal_events.push(ModelEvent::Completed { result: result.clone() });
        self.events.extend(terminal_events.iter().cloned());
        Ok((terminal_events, result))
    }

    fn parse_block(&mut self, bytes: &[u8]) -> Result<Vec<ModelEvent>, ModelError> {
        let block = std::str::from_utf8(bytes).map_err(|error| ModelError::Protocol {
            message: error.to_string(),
        })?;
        let data = block
            .lines()
            .filter_map(|line| line.trim_end_matches('\r').strip_prefix("data:").map(str::trim))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            return Ok(Vec::new());
        }

        let value: serde_json::Value = serde_json::from_str(&data).map_err(|error| ModelError::Protocol {
            message: format!("invalid chunk: {error}"),
        })?;

        if let Some(error) = value.get("error") {
            return Err(ModelError::BackendError {
                message: error["message"].as_str().unwrap_or("Gemini stream error").to_string(),
                code: error["status"].as_str().unwrap_or("stream_error").to_string(),
            });
        }

        let mut events = Vec::new();

        if let Some(usage_value) = value.get("usageMetadata") {
            if let Ok(raw) = serde_json::from_value::<RawGeminiUsage>(usage_value.clone()) {
                self.usage = GeminiUsageMapper::map_usage(&raw);
                events.push(ModelEvent::UsageUpdate { usage: self.usage.clone() });
            }
        }

        let Some(candidate) = value.get("candidates").and_then(|c| c.as_array()).and_then(|c| c.first()) else {
            return Ok(events);
        };

        if let Some(reason) = candidate.get("finishReason").and_then(|r| r.as_str()) {
            self.stop_reason = reason.to_string();
            self.saw_finish_reason = true;
        }

        let parts = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array());
        if let Some(parts) = parts {
            for part in parts {
                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                    if !text.is_empty() {
                        events.push(ModelEvent::TextDelta { delta: text.to_string() });
                    }
                }
                if let Some(function_call) = part.get("functionCall") {
                    let name = function_call["name"].as_str().unwrap_or_default().to_string();
                    let args = function_call.get("args").cloned().unwrap_or_else(|| serde_json::json!({}));
                    // Gemini sends the whole function call in one shot — no
                    // provider call ID exists, so the internal ToolCallId is
                    // minted fresh right here and never needs to be mapped
                    // back to anything (see module docs).
                    let id = ToolCallId::new();
                    events.push(ModelEvent::ToolCallStarted { id, name: name.clone() });
                    events.push(ModelEvent::ToolCallCompleted { id, name, input: args });
                }
            }
        }

        Ok(events)
    }
}

fn find_sse_boundary(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|window| window == b"\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M4/M4.5: proves `GeminiGenerationConfig`'s JSON field names match the
    /// `generateContent` API's `camelCase` expectations
    /// (`maxOutputTokens`/`stopSequences`, via `#[serde(rename_all = ...)]`
    /// wherever that's declared on the struct) — same rationale as the
    /// equivalent OpenAI wire-shape test: the M4.1 contract suite proves
    /// `ModelRequest` carries the right values up to this boundary, not that
    /// this boundary serializes them the way the real API expects.
    #[test]
    fn generation_config_serializes_with_the_expected_gemini_field_names() {
        let config = GeminiGenerationConfig {
            max_output_tokens: Some(2048),
            temperature: Some(0.7),
            stop_sequences: Some(vec!["STOP".to_string()]),
        };
        let json = serde_json::to_value(&config).expect("serialize GeminiGenerationConfig");
        assert_eq!(json["maxOutputTokens"], 2048);
        assert_eq!(json["temperature"], 0.7);
        assert_eq!(json["stopSequences"], serde_json::json!(["STOP"]));

        let bare = GeminiGenerationConfig {
            max_output_tokens: None,
            temperature: None,
            stop_sequences: None,
        };
        let bare_json = serde_json::to_value(&bare).expect("serialize bare GeminiGenerationConfig");
        assert!(bare_json.get("maxOutputTokens").is_none());
        assert!(bare_json.get("temperature").is_none());
        assert!(bare_json.get("stopSequences").is_none());
    }

    fn user_message(content: Vec<ContentBlock>) -> AgentMessage {
        AgentMessage {
            id: harness_protocol::ids::MessageId::new(),
            role: MessageRole::User,
            content,
            created_at: harness_protocol::ids::Timestamp::now(),
        }
    }

    /// M4: an image content block must convert into a real `inlineData`
    /// part, not be silently dropped — matching Anthropic's existing image
    /// pass-through, previously Gemini-specific wire support for it did not
    /// exist even though the client already advertised `images: true` in
    /// its capabilities.
    #[test]
    fn an_image_block_becomes_a_real_inline_data_part_not_silently_dropped() {
        let message = user_message(vec![
            ContentBlock::Text { text: "what is this?".into() },
            ContentBlock::Image { mime_type: "image/png".into(), data: vec![1, 2, 3] },
        ]);
        let contents = convert_messages(std::slice::from_ref(&message));
        assert_eq!(contents.len(), 1);
        let json = serde_json::to_value(&contents[0]).expect("serialize GeminiContent");
        let parts = json["parts"].as_array().expect("parts must be an array");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["text"], "what is this?");
        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
        assert_eq!(
            parts[1]["inlineData"]["data"],
            base64::engine::general_purpose::STANDARD.encode([1, 2, 3])
        );
    }

    const FIXTURE: &str = "\
data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Hello, \"}]}}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":1,\"totalTokenCount\":11}}\n\n\
data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"world!\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":5,\"totalTokenCount\":15}}\n\n";

    #[test]
    fn incremental_parser_handles_single_byte_chunks() {
        let mut parser = GeminiSseParser::new("gemini-1.5-pro".to_string());
        let mut events = Vec::new();
        for byte in FIXTURE.as_bytes() {
            events.extend(parser.push_chunk(std::slice::from_ref(byte)).expect("valid chunk"));
        }
        let (terminal, result) = parser.finish().expect("complete fixture");
        events.extend(terminal);

        let text: String = events
            .iter()
            .filter_map(|e| match e {
                ModelEvent::TextDelta { delta } => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello, world!");
        assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
        assert_eq!(result.stop_reason, "STOP");
        assert_eq!(result.usage.input_tokens.value(), Some(10));
        assert_eq!(result.usage.output_tokens.value(), Some(5));
        assert_eq!(result.usage.total_tokens.value(), Some(15));
    }

    #[test]
    fn parser_rejects_a_stream_without_finish_reason() {
        let mut parser = GeminiSseParser::new("gemini-1.5-pro".to_string());
        parser
            .push_chunk(b"data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"hi\"}]}}]}\n\n")
            .expect("chunk parses");
        assert!(matches!(parser.finish(), Err(ModelError::Protocol { .. })));
    }

    #[test]
    fn function_call_arrives_whole_and_completes_immediately() {
        let mut parser = GeminiSseParser::new("gemini-1.5-pro".to_string());
        let chunk = b"data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"name\":\"get_weather\",\"args\":{\"city\":\"paris\"}}}]},\"finishReason\":\"STOP\"}]}\n\n";
        let events = parser.push_chunk(chunk).expect("valid chunk");
        let completed = events
            .iter()
            .find_map(|e| match e {
                ModelEvent::ToolCallCompleted { name, input, .. } => Some((name.clone(), input.clone())),
                _ => None,
            })
            .expect("a ToolCallCompleted event on the very first chunk");
        assert_eq!(completed.0, "get_weather");
        assert_eq!(completed.1, serde_json::json!({ "city": "paris" }));
    }

    #[test]
    fn multiple_parts_in_one_chunk_each_produce_an_event() {
        let mut parser = GeminiSseParser::new("gemini-1.5-pro".to_string());
        let chunk = b"data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"checking...\"},{\"functionCall\":{\"name\":\"get_weather\",\"args\":{}}}]},\"finishReason\":\"STOP\"}]}\n\n";
        let events = parser.push_chunk(chunk).expect("valid chunk");
        assert!(events.iter().any(|e| matches!(e, ModelEvent::TextDelta { delta } if delta == "checking...")));
        assert!(events.iter().any(|e| matches!(e, ModelEvent::ToolCallCompleted { .. })));
    }
}
