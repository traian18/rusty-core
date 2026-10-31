//! One Chat Completions frame: bookkeeping from its metadata, then the event its delta stands for.

use rusty_model_backend::events::{ModelError, ModelEvent};
use rusty_protocol::ids::ToolCallId;
use serde_json::Value;

use super::{OpenAiSseParser, ToolBuffer};
use crate::usage::{OpenAiUsageMapper, RawOpenAiUsage};

impl OpenAiSseParser {
    /// Turns one frame into at most one event. `[DONE]` ends the stream.
    pub(super) fn parse_block(&mut self, bytes: &[u8]) -> Result<Option<ModelEvent>, ModelError> {
        let text = std::str::from_utf8(bytes).map_err(|error| ModelError::Protocol {
            message: error.to_string(),
        })?;
        let data = data_lines(text);
        if data.is_empty() {
            return Ok(None);
        }
        if data == "[DONE]" {
            self.saw_done = true;
            return Ok(None);
        }
        let value: Value = serde_json::from_str(&data).map_err(|error| ModelError::Protocol {
            message: format!("invalid chunk: {error}"),
        })?;

        self.note_metadata(&value)?;
        let Some(choice) = value["choices"]
            .as_array()
            .and_then(|choices| choices.first())
        else {
            return Ok(self.usage_only_event(&value));
        };
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.stop_reason = reason.to_string();
        }
        let delta = &choice["delta"];
        Ok(reasoning_event(delta)
            .or_else(|| text_event(delta))
            .or_else(|| self.tool_call_event(delta)))
    }

    /// Records the response id, model and usage a frame carries, and fails on an error frame.
    fn note_metadata(&mut self, value: &Value) -> Result<(), ModelError> {
        if self.response_id.is_none() {
            self.response_id = value["id"].as_str().map(str::to_owned);
        }
        if let Some(error) = value.get("error") {
            return Err(ModelError::BackendError {
                message: error["message"]
                    .as_str()
                    .unwrap_or("OpenAI stream error")
                    .to_string(),
                code: error["type"].as_str().unwrap_or("stream_error").to_string(),
            });
        }
        if self.model.is_empty() {
            if let Some(model) = value["model"].as_str() {
                self.model = model.to_string();
            }
        }
        let raw_usage = value
            .get("usage")
            .filter(|usage| !usage.is_null())
            .and_then(|usage| serde_json::from_value::<RawOpenAiUsage>(usage.clone()).ok());
        if let Some(raw) = raw_usage {
            self.usage = OpenAiUsageMapper::map_usage(&raw);
        }
        Ok(())
    }

    /// A frame with no choices can still carry the final usage; report it once.
    fn usage_only_event(&mut self, value: &Value) -> Option<ModelEvent> {
        let carries_usage = value.get("usage").is_some_and(|usage| !usage.is_null());
        if !(carries_usage && self.has_usage()) {
            return None;
        }
        self.usage_event_sent = true;
        Some(ModelEvent::UsageUpdate {
            usage: self.usage.clone(),
        })
    }

    /// The first tool call of a delta: a start when its index is new, an argument fragment after.
    fn tool_call_event(&mut self, delta: &Value) -> Option<ModelEvent> {
        let tool_call = delta["tool_calls"].as_array()?.first()?;
        let index = tool_call["index"].as_u64().unwrap_or(0) as usize;
        let fragment = tool_call["function"]["arguments"]
            .as_str()
            .unwrap_or_default();
        if let Some(buffer) = self.tools.get_mut(&index) {
            buffer.json.push_str(fragment);
            return (!fragment.is_empty()).then(|| ModelEvent::ToolCallDelta {
                id: buffer.id,
                delta: fragment.to_string(),
            });
        }
        let id = ToolCallId::new();
        if let Some(provider_id) = tool_call["id"].as_str() {
            self.tool_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id, provider_id.to_string());
        }
        let name = tool_call["function"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        self.tools.insert(
            index,
            ToolBuffer {
                id,
                name: name.clone(),
                json: fragment.to_string(),
            },
        );
        Some(ModelEvent::ToolCallStarted { id, name })
    }
}

fn data_lines(text: &str) -> String {
    text.lines()
        .filter_map(|line| {
            line.trim_end_matches('\r')
                .strip_prefix("data:")
                .map(str::trim)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// DeepSeek puts thinking in `delta.reasoning_content`; some other gateways use
/// `delta.reasoning`. Plain OpenAI has neither, so this finds nothing there.
fn reasoning_event(delta: &Value) -> Option<ModelEvent> {
    let text = delta["reasoning_content"]
        .as_str()
        .or_else(|| delta["reasoning"].as_str())
        .filter(|text| !text.is_empty())?;
    Some(ModelEvent::ReasoningDelta {
        delta: text.to_string(),
    })
}

fn text_event(delta: &Value) -> Option<ModelEvent> {
    let text = delta["content"].as_str().filter(|text| !text.is_empty())?;
    Some(ModelEvent::TextDelta {
        delta: text.to_string(),
    })
}
