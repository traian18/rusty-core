//! Converting the conversation and tools into Messages API shapes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;

use rusty_protocol::ids::ToolCallId;
use rusty_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use rusty_protocol::tools::ToolDescriptor;

use super::request::{
    AnthropicContentBlock, AnthropicImageSource, AnthropicMessage, AnthropicRole, AnthropicTool,
};

/// The wire form of one message, or `None` for a system message: system text travels in the
/// request's `system` field, not in the message list.
pub fn agent_message_to_anthropic(message: &AgentMessage) -> Option<AnthropicMessage> {
    let role = match message.role {
        MessageRole::User | MessageRole::Tool => AnthropicRole::User,
        MessageRole::Assistant => AnthropicRole::Assistant,
        MessageRole::System => return None,
    };
    let content = message
        .content
        .iter()
        .filter_map(|block| match block {
            // The Messages API rejects a text block with no text ("text
            // content blocks must be non-empty"); it carries nothing anyway.
            ContentBlock::Text { text } if text.is_empty() => None,
            ContentBlock::Text { text } => Some(AnthropicContentBlock::Text { text: text.clone() }),
            ContentBlock::ToolUse { call } => Some(AnthropicContentBlock::ToolUse {
                id: call.id.to_string(),
                name: call.name.clone(),
                input: call.arguments.clone(),
            }),
            ContentBlock::ToolResult { call_id, result } => {
                Some(AnthropicContentBlock::ToolResult {
                    tool_use_id: call_id.to_string(),
                    content: result.output_preview.clone(),
                })
            }
            ContentBlock::Image { mime_type, data } => Some(AnthropicContentBlock::Image {
                source: AnthropicImageSource {
                    kind: "base64",
                    media_type: mime_type.clone(),
                    data: base64::engine::general_purpose::STANDARD.encode(data),
                },
            }),
        })
        .collect();
    Some(AnthropicMessage { role, content })
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
        .filter_map(agent_message_to_anthropic)
        // An assistant message left with no content blocks (a turn that
        // only produced reasoning, recorded by an older core) says nothing,
        // and the Messages API rejects empty `content` on the next request.
        // The core no longer produces such messages; persisted transcripts
        // can still carry one.
        .filter(|message| {
            !matches!(message.role, AnthropicRole::Assistant) || !message.content.is_empty()
        })
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
    for block in converted
        .iter_mut()
        .flat_map(|message| &mut message.content)
    {
        restore_provider_id(block, tool_ids);
    }
    converted
}

/// Puts the provider's own id back on a tool use or result that carries the harness-internal one.
fn restore_provider_id(block: &mut AnthropicContentBlock, tool_ids: &HashMap<ToolCallId, String>) {
    let wire_id = match block {
        AnthropicContentBlock::ToolUse { id, .. } => id,
        AnthropicContentBlock::ToolResult { tool_use_id, .. } => tool_use_id,
        AnthropicContentBlock::Text { .. } | AnthropicContentBlock::Image { .. } => return,
    };
    let provider_id = wire_id
        .parse::<ToolCallId>()
        .ok()
        .and_then(|internal_id| tool_ids.get(&internal_id));
    if let Some(provider_id) = provider_id {
        *wire_id = provider_id.clone();
    }
}
