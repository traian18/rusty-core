//! OpenAI Responses API wire types and incremental SSE normalization.
//!
//! Ported from the TypeScript implementation of the same protocol so that OpenCode Zen's GPT,
//! Grok and Muse Spark model family (which has no CORS support and cannot run from the browser)
//! can execute from Rust. `request` holds the JSON bodies, `convert` builds them from the
//! conversation, and `sse` turns the response stream into model events.

mod convert;
mod request;
mod sse;

pub use convert::{
    build_system_message, convert_messages_with_tool_ids, tool_descriptor_to_responses,
    ProviderToolIds,
};
pub use request::{
    reasoning_effort_to_responses, OpenAiResponsesRequest, ResponsesInputContentPart,
    ResponsesInputItem, ResponsesOutputContentPart, ResponsesReasoning, ResponsesTool,
    RESPONSES_MIN_OUTPUT_TOKENS,
};
pub use sse::OpenAiResponsesSseParser;

#[cfg(test)]
mod tests;
