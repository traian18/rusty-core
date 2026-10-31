//! The JSON bodies sent to the Chat Completions API.

use serde::Serialize;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<OpenAiResponseFormat>,
    /// OpenAI's own Chat Completions param for o-series/gpt-5 reasoning
    /// models (`"low"`/`"medium"`/`"high"`), also accepted verbatim by
    /// several OpenAI-compatible gateways. Only ever `Some` when the
    /// configured endpoint opted in via `OpenAiConfig::supports_reasoning`
    /// -- see that field's own doc comment for why this isn't unconditional.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub stream: bool,
    pub stream_options: StreamOptions,
}

/// Maps rusty-core's `ReasoningEffort` onto the Chat Completions
/// `reasoning_effort` string values -- same 1:1 mapping as
/// `rusty-provider-openai-responses`'s own `reasoning_effort_to_responses`.
pub fn reasoning_effort_to_openai(effort: rusty_protocol::backend::ReasoningEffort) -> String {
    effort.openai_wire_value().to_string()
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

/// Chat Completions' `response_format`.
///
/// `json_object` requires the word "JSON" to appear somewhere in the prompt
/// or the API rejects the call; `json_schema` does not, and additionally
/// enforces the schema when `strict` is set.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenAiResponseFormat {
    Text,
    JsonObject,
    JsonSchema { json_schema: OpenAiJsonSchema },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OpenAiJsonSchema {
    pub name: String,
    pub schema: serde_json::Value,
    pub strict: bool,
}

impl OpenAiResponseFormat {
    /// Translate the provider-neutral format. Returns `None` for
    /// `ResponseFormat::Text` so the field is omitted entirely rather than
    /// sent as an explicit `{"type":"text"}` — some OpenAI-compatible
    /// servers reject fields they don't implement, and text is their default
    /// anyway.
    pub fn from_neutral(format: &rusty_protocol::backend::ResponseFormat) -> Option<Self> {
        use rusty_protocol::backend::ResponseFormat;
        match format {
            ResponseFormat::Text => None,
            ResponseFormat::JsonObject => Some(Self::JsonObject),
            ResponseFormat::JsonSchema {
                name,
                schema,
                strict,
            } => Some(Self::JsonSchema {
                json_schema: OpenAiJsonSchema {
                    name: name.clone(),
                    schema: schema.clone(),
                    strict: *strict,
                },
            }),
        }
    }
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
