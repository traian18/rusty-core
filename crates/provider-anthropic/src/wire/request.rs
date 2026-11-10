//! The JSON bodies sent to the Messages API.

use serde::Serialize;

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
    /// Carries the effort level for adaptive thinking -- see
    /// [`resolve_adaptive_thinking`](super::thinking::resolve_adaptive_thinking).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<AnthropicOutputConfig>,
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
    format: &rusty_protocol::backend::ResponseFormat,
) -> Option<(AnthropicTool, AnthropicToolChoice)> {
    use rusty_protocol::backend::ResponseFormat;
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
    /// Present for `"enabled"` thinking only; `"adaptive"` thinking takes no
    /// budget and is steered by [`AnthropicOutputConfig::effort`] instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnthropicOutputConfig {
    pub effort: &'static str,
}
