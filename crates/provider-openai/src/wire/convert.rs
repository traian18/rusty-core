//! Converting the conversation and tools into Chat Completions messages.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;

use rusty_protocol::ids::ToolCallId;
use rusty_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use rusty_protocol::tools::ToolDescriptor;

use super::request::*;

/// Converts one message's content blocks into an [`OpenAiContent`]: plain
/// text when there are no images (identical to the pre-image-support wire
/// shape), or a typed parts array once at least one `ContentBlock::Image` is
/// present, since OpenAI only accepts the array form for multimodal content.
pub(super) fn content_blocks_to_openai_content(content: &[ContentBlock]) -> OpenAiContent {
    let has_image = content
        .iter()
        .any(|block| matches!(block, ContentBlock::Image { .. }));
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

pub(super) fn concat_text(content: &[ContentBlock]) -> String {
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
pub(super) fn agent_message_to_openai(
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
            // An assistant message with neither text nor tool calls says
            // nothing, and the wire form it would take -- `{"role":
            // "assistant"}` -- is rejected outright by several
            // Chat-Completions-compatible providers (Cohere via OpenRouter:
            // "must have non-empty content or tool calls"). The core no
            // longer produces such messages, but transcripts persisted
            // before it stopped can still carry one.
            if text.is_empty() && tool_calls.is_empty() {
                return Vec::new();
            }
            vec![OpenAiMessage {
                role: "assistant".to_string(),
                content: if text.is_empty() {
                    None
                } else {
                    Some(OpenAiContent::Text(text))
                },
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                tool_call_id: None,
            }]
        }
        MessageRole::System => unreachable!("system messages must be filtered before conversion"),
    }
}

/// Builds the system-role message (if any) the same way Anthropic's client
/// does: prefer the request's `system_prompt` field, falling back to
/// concatenating any `System`-role messages in the transcript.
pub fn build_system_message(
    system_prompt: &str,
    messages: &[AgentMessage],
) -> Option<OpenAiMessage> {
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
