//! OpenAI Chat Completions wire types and incremental SSE normalization.
//!
//! `request` holds the JSON bodies, `convert` builds them from the conversation, and `sse` turns
//! the response stream into model events.

mod convert;
mod request;
mod sse;

pub use convert::{
    build_system_message, convert_messages_with_tool_ids, tool_descriptor_to_openai,
    ProviderToolIds,
};
pub use request::{
    reasoning_effort_to_openai, OpenAiContent, OpenAiContentPart, OpenAiFunctionCall,
    OpenAiFunctionDef, OpenAiImageUrl, OpenAiJsonSchema, OpenAiMessage, OpenAiRequest,
    OpenAiResponseFormat, OpenAiTool, OpenAiToolCallOut, StreamOptions,
};
pub use sse::OpenAiSseParser;

#[cfg(test)]
mod tests;
