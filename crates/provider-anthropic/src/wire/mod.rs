//! Anthropic request wire types and incremental SSE normalization.
//!
//! `request` holds the JSON bodies, `convert` builds them from the conversation, `thinking` picks
//! the thinking mode a model accepts, and `sse` turns the response stream into model events.

mod convert;
mod request;
mod sse;
mod thinking;

pub use convert::{
    agent_message_to_anthropic, build_system, convert_messages, convert_messages_with_tool_ids,
    find_invalid_tool_names, is_valid_anthropic_tool_name, tool_descriptor_to_anthropic,
    ProviderToolIds,
};
pub use request::{
    structured_output_tool, AnthropicContentBlock, AnthropicImageSource, AnthropicMessage,
    AnthropicOutputConfig, AnthropicRequest, AnthropicRole, AnthropicThinking, AnthropicTool,
    AnthropicToolChoice, STRUCTURED_OUTPUT_TOOL,
};
pub use sse::AnthropicSseParser;
pub use thinking::{
    is_adaptive_thinking_rejection, requires_adaptive_thinking, resolve_adaptive_thinking,
    resolve_thinking,
};

#[cfg(test)]
mod tests;
