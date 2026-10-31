//! One SSE block: split into its event name and data, then handled by kind.

use rusty_model_backend::events::{ModelError, ModelEvent};
use rusty_protocol::ids::ToolCallId;
use serde_json::{json, Value};

use super::{
    fields::{block_index, initial_input, text_of},
    usage::{map_usage, merge_usage, RawUsage},
    AnthropicSseParser, ToolBuffer,
};
use crate::wire::request::STRUCTURED_OUTPUT_TOOL;

/// The `event:` name and the joined `data:` lines of a block.
struct SseBlock<'a> {
    event: Option<&'a str>,
    data: String,
}

impl<'a> SseBlock<'a> {
    fn parse(text: &'a str) -> Self {
        let event = text.lines().find_map(|line| field(line, "event:"));
        let data = text
            .lines()
            .filter_map(|line| field(line, "data:"))
            .collect::<Vec<_>>()
            .join("\n");
        Self { event, data }
    }
}

fn field<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    line.trim_end_matches('\r')
        .strip_prefix(prefix)
        .map(str::trim)
}

impl AnthropicSseParser {
    /// Turns one block into at most one event. `message_stop` and `[DONE]` end the stream.
    pub(super) fn parse_block(&mut self, bytes: &[u8]) -> Result<Option<ModelEvent>, ModelError> {
        let text = std::str::from_utf8(bytes).map_err(|error| ModelError::Protocol {
            message: error.to_string(),
        })?;
        let block = SseBlock::parse(text);
        if block.event == Some("message_stop") || block.data.trim() == "[DONE]" {
            self.saw_message_stop = true;
            return Ok(None);
        }
        if block.data.trim().is_empty() {
            return Ok(None);
        }
        let value: Value =
            serde_json::from_str(&block.data).map_err(|error| ModelError::Protocol {
                message: format!("invalid event data: {error}"),
            })?;

        match block.event.or_else(|| value["type"].as_str()).unwrap_or("") {
            "message_start" => Ok(self.on_message_start(&value)),
            "content_block_start" => Ok(self.on_content_block_start(&value)),
            "content_block_delta" => Ok(self.on_content_block_delta(&value)),
            "content_block_stop" => self.on_content_block_stop(&value),
            "message_delta" => Ok(self.on_message_delta(&value)),
            "message_stop" => {
                self.saw_message_stop = true;
                Ok(None)
            }
            "error" => Err(stream_error(&value)),
            _ => Ok(None),
        }
    }

    fn on_message_start(&mut self, value: &Value) -> Option<ModelEvent> {
        self.model = text_of(&value["message"]["model"]);
        let raw = serde_json::from_value::<RawUsage>(value["message"]["usage"].clone()).ok()?;
        self.usage = map_usage(&raw);
        Some(ModelEvent::UsageUpdate {
            usage: self.usage.clone(),
        })
    }

    /// Only tool-use blocks need tracking. The structured-output tool is an implementation detail
    /// of `response_format`: it is never registered with the host, so it surfaces as text, not as
    /// a tool call.
    fn on_content_block_start(&mut self, value: &Value) -> Option<ModelEvent> {
        let index = block_index(value);
        let content = &value["content_block"];
        if content["type"] != "tool_use" {
            return None;
        }
        if content["name"] == STRUCTURED_OUTPUT_TOOL {
            self.structured_output_blocks.insert(index);
            return initial_input(content).map(|delta| ModelEvent::TextDelta { delta });
        }
        let id = ToolCallId::new();
        self.remember_provider_id(id, content["id"].as_str());
        let name = text_of(&content["name"]);
        self.tools.insert(
            index,
            ToolBuffer {
                id,
                name: name.clone(),
                json: initial_input(content).unwrap_or_default(),
            },
        );
        Some(ModelEvent::ToolCallStarted { id, name })
    }

    fn on_content_block_delta(&mut self, value: &Value) -> Option<ModelEvent> {
        let index = block_index(value);
        let delta = &value["delta"];
        match delta["type"].as_str() {
            Some("text_delta") => Some(ModelEvent::TextDelta {
                delta: text_of(&delta["text"]),
            }),
            Some("thinking_delta") => Some(ModelEvent::ReasoningDelta {
                delta: text_of(&delta["thinking"]),
            }),
            Some("input_json_delta") if self.structured_output_blocks.contains(&index) => {
                Some(ModelEvent::TextDelta {
                    delta: text_of(&delta["partial_json"]),
                })
            }
            Some("input_json_delta") => {
                let tool = self.tools.get_mut(&index)?;
                let fragment = text_of(&delta["partial_json"]);
                tool.json.push_str(&fragment);
                Some(ModelEvent::ToolCallDelta {
                    id: tool.id,
                    delta: fragment,
                })
            }
            _ => None,
        }
    }

    /// A structured-output block was already emitted as text; a tool block is complete and its
    /// assembled JSON is parsed.
    fn on_content_block_stop(&mut self, value: &Value) -> Result<Option<ModelEvent>, ModelError> {
        let index = block_index(value);
        if self.structured_output_blocks.remove(&index) {
            return Ok(None);
        }
        let Some(tool) = self.tools.remove(&index) else {
            return Ok(None);
        };
        let input = if tool.json.is_empty() {
            json!({})
        } else {
            serde_json::from_str(&tool.json).map_err(|error| ModelError::Protocol {
                message: format!("invalid tool input: {error}"),
            })?
        };
        Ok(Some(ModelEvent::ToolCallCompleted {
            id: tool.id,
            name: tool.name,
            input,
        }))
    }

    fn on_message_delta(&mut self, value: &Value) -> Option<ModelEvent> {
        if let Some(reason) = value["delta"]["stop_reason"].as_str() {
            self.stop_reason = reason.to_string();
        }
        let raw = serde_json::from_value::<RawUsage>(value["usage"].clone()).ok()?;
        self.usage = merge_usage(&self.usage, &raw);
        Some(ModelEvent::UsageUpdate {
            usage: self.usage.clone(),
        })
    }

    /// Remembers the provider's own id for a tool call, which the next request must echo back.
    pub(super) fn remember_provider_id(&self, id: ToolCallId, provider_id: Option<&str>) {
        if let Some(provider_id) = provider_id {
            self.tool_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id, provider_id.to_string());
        }
    }
}

fn stream_error(value: &Value) -> ModelError {
    ModelError::BackendError {
        message: value["error"]["message"]
            .as_str()
            .unwrap_or("Anthropic stream error")
            .to_string(),
        code: value["error"]["type"]
            .as_str()
            .unwrap_or("stream_error")
            .to_string(),
    }
}
