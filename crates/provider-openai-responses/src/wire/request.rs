//! The JSON bodies sent to the Responses API.

use serde::Serialize;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct OpenAiResponsesRequest {
    pub model: String,
    pub input: Vec<ResponsesInputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ResponsesTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ResponsesReasoning>,
    pub stream: bool,
    /// Always `false` -- this client never relies on OpenAI's own
    /// server-side conversation state (no multi-turn `previous_response_id`
    /// chaining); the full transcript is resent every turn, same as every
    /// other integration in this workspace.
    pub store: bool,
}

/// The Responses API's `reasoning.effort` request param.
/// <https://platform.openai.com/docs/api-reference/responses/create>
#[derive(Debug, Clone, Serialize)]
pub struct ResponsesReasoning {
    pub effort: String,
}

/// Maps rusty-core's `ReasoningEffort` onto the Responses API's own
/// `reasoning.effort` string (see `ReasoningEffort::openai_wire_value`).
pub fn reasoning_effort_to_responses(
    effort: rusty_protocol::backend::ReasoningEffort,
) -> ResponsesReasoning {
    ResponsesReasoning {
        effort: effort.openai_wire_value().to_string(),
    }
}

/// The minimum `max_output_tokens` the Responses API accepts.
/// <https://github.com/earendil-works/pi/issues/6265>
pub const RESPONSES_MIN_OUTPUT_TOKENS: u64 = 16;

/// One item of the Responses API's `input` array. Unlike Chat Completions'
/// flat `messages` array, `input` mixes plain `{role, content}` messages
/// with typed function-call/function-call-output items.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ResponsesInputItem {
    /// A `user`/`system` message -- no `type` tag, matching the Responses
    /// API's "easy input message" shape.
    InputMessage {
        role: String,
        content: Vec<ResponsesInputContentPart>,
    },
    /// An `assistant` message, which the Responses API represents as an
    /// explicitly-tagged output item even when fed back in as input.
    OutputMessage {
        #[serde(rename = "type")]
        kind: String,
        role: String,
        content: Vec<ResponsesOutputContentPart>,
    },
    FunctionCall {
        #[serde(rename = "type")]
        kind: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        #[serde(rename = "type")]
        kind: String,
        call_id: String,
        output: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesInputContentPart {
    InputText { text: String },
    InputImage { detail: String, image_url: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesOutputContentPart {
    OutputText {
        text: String,
        annotations: Vec<serde_json::Value>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponsesTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub strict: bool,
}
