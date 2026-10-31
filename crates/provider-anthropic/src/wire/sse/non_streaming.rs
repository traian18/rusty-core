//! A gateway that answers with one complete JSON message instead of an SSE stream.

use rusty_model_backend::events::ModelEvent;
use rusty_protocol::ids::ToolCallId;
use serde_json::{json, Value};

use super::{
    usage::{map_usage, RawUsage},
    AnthropicSseParser,
};
use crate::wire::request::STRUCTURED_OUTPUT_TOOL;

impl AnthropicSseParser {
    /// The events a complete `{"type":"message", ...}` response stands for: usage first, then
    /// each content block in order.
    pub(super) fn parse_non_streaming_json(&mut self, value: &Value) -> Vec<ModelEvent> {
        let mut events = Vec::new();
        if let Some(model) = value["model"].as_str() {
            self.model = model.to_string();
        }
        if let Some(reason) = value["stop_reason"].as_str() {
            self.stop_reason = reason.to_string();
        }
        if let Ok(raw) = serde_json::from_value::<RawUsage>(value["usage"].clone()) {
            self.usage = map_usage(&raw);
            events.push(ModelEvent::UsageUpdate {
                usage: self.usage.clone(),
            });
        }
        for block in value["content"].as_array().into_iter().flatten() {
            events.extend(self.block_events(block));
        }
        events
    }

    fn block_events(&self, block: &Value) -> Vec<ModelEvent> {
        match block["type"].as_str().unwrap_or("") {
            "text" => block["text"]
                .as_str()
                .map(|text| ModelEvent::TextDelta {
                    delta: text.to_string(),
                })
                .into_iter()
                .collect(),
            "thinking" => block["thinking"]
                .as_str()
                .map(|thinking| ModelEvent::ReasoningDelta {
                    delta: thinking.to_string(),
                })
                .into_iter()
                .collect(),
            "tool_use" => self.tool_use_events(block),
            _ => Vec::new(),
        }
    }

    /// The structured-output tool becomes text; any other tool is a started and completed call.
    fn tool_use_events(&self, block: &Value) -> Vec<ModelEvent> {
        let name = block["name"].as_str().unwrap_or_default();
        let input = block.get("input").cloned().unwrap_or(json!({}));
        if name == STRUCTURED_OUTPUT_TOOL {
            return vec![ModelEvent::TextDelta {
                delta: input.to_string(),
            }];
        }
        let id = ToolCallId::new();
        self.remember_provider_id(id, block["id"].as_str());
        vec![
            ModelEvent::ToolCallStarted {
                id,
                name: name.to_string(),
            },
            ModelEvent::ToolCallCompleted {
                id,
                name: name.to_string(),
                input,
            },
        ]
    }
}
