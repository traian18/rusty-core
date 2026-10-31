//! Converting the conversation and tools into Responses API input items.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;

use rusty_protocol::ids::ToolCallId;
use rusty_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use rusty_protocol::tools::ToolDescriptor;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------
use super::request::{
    ResponsesInputContentPart, ResponsesInputItem, ResponsesOutputContentPart, ResponsesTool,
};

pub fn tool_descriptor_to_responses(tool: &ToolDescriptor) -> ResponsesTool {
    ResponsesTool {
        kind: "function".to_string(),
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.input_schema.clone(),
        strict: false,
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

pub(super) fn content_blocks_to_responses_input(
    content: &[ContentBlock],
) -> Vec<ResponsesInputContentPart> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => {
                Some(ResponsesInputContentPart::InputText { text: text.clone() })
            }
            ContentBlock::Image { mime_type, data } => {
                Some(ResponsesInputContentPart::InputImage {
                    detail: "auto".to_string(),
                    image_url: format!(
                        "data:{mime_type};base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(data)
                    ),
                })
            }
            _ => None,
        })
        .collect()
}

/// Provider tool-call IDs (`"call_xxx"`) keyed by the harness-internal
/// `ToolCallId`, same role as `rusty-provider-openai`'s own
/// `ProviderToolIds` -- lets a tool call minted while parsing a streamed
/// response be translated back to the ID the Responses API expects when
/// that call reappears in a later turn's history.
pub type ProviderToolIds = Arc<Mutex<HashMap<ToolCallId, String>>>;

pub(super) fn agent_message_to_responses(
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
