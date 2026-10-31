//! One Responses SSE frame: its JSON payload, dispatched by event type.

use rusty_model_backend::events::{ModelError, ModelEvent, ModelResult};
use rusty_protocol::ids::ToolCallId;
use serde_json::{json, Value};

use super::{OpenAiResponsesSseParser, ToolBuffer};
use crate::usage::{OpenAiResponsesUsageMapper, RawOpenAiResponsesUsage};

impl OpenAiResponsesSseParser {
    /// Turns one frame into the events it stands for (none for frames that carry nothing new).
    pub(super) fn parse_block(&mut self, bytes: &[u8]) -> Result<Vec<ModelEvent>, ModelError> {
        let text = std::str::from_utf8(bytes).map_err(|error| ModelError::Protocol {
            message: error.to_string(),
        })?;
        let data = data_lines(text);
        if data.is_empty() {
            return Ok(vec![]);
        }
        let value: Value = serde_json::from_str(&data).map_err(|error| ModelError::Protocol {
            message: format!("invalid Responses SSE frame: {error}"),
        })?;

        match value["type"].as_str().unwrap_or("") {
            "response.output_text.delta" | "response.refusal.delta" => {
                Ok(delta_event(&value, |delta| ModelEvent::TextDelta { delta }))
            }
            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                Ok(delta_event(&value, |delta| ModelEvent::ReasoningDelta {
                    delta,
                }))
            }
            "response.output_item.added" => Ok(self.on_function_call_added(&value)),
            "response.function_call_arguments.delta" => Ok(self.on_arguments_delta(&value)),
            "response.output_item.done" => self.on_function_call_done(&value),
            "response.completed" => Ok(self.on_response_finished(&value, "stop")),
            "response.incomplete" => Ok(self.on_response_finished(&value, "length")),
            "response.failed" => Err(self.on_response_failed(&value)),
            "error" => Err(stream_error(&value)),
            _ => Ok(vec![]),
        }
    }

    fn on_function_call_added(&mut self, value: &Value) -> Vec<ModelEvent> {
        let item = &value["item"];
        if item["type"] != "function_call" {
            return vec![];
        }
        let id = ToolCallId::new();
        if let Some(provider_call_id) = item["call_id"].as_str() {
            self.tool_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id, provider_call_id.to_string());
        }
        let name = item["name"].as_str().unwrap_or_default().to_string();
        self.tool_buffers.insert(
            output_index(value),
            ToolBuffer {
                id,
                name: name.clone(),
            },
        );
        self.saw_function_call = true;
        vec![ModelEvent::ToolCallStarted { id, name }]
    }

    fn on_arguments_delta(&self, value: &Value) -> Vec<ModelEvent> {
        let Some(buffer) = self.tool_buffers.get(&output_index(value)) else {
            return vec![];
        };
        delta_event(value, |delta| ModelEvent::ToolCallDelta {
            id: buffer.id,
            delta,
        })
    }

    fn on_function_call_done(&mut self, value: &Value) -> Result<Vec<ModelEvent>, ModelError> {
        let item = &value["item"];
        if item["type"] != "function_call" {
            return Ok(vec![]);
        }
        let Some(buffer) = self.tool_buffers.remove(&output_index(value)) else {
            return Ok(vec![]);
        };
        let arguments = item["arguments"].as_str().unwrap_or("{}");
        let input = if arguments.is_empty() {
            json!({})
        } else {
            serde_json::from_str(arguments).map_err(|error| ModelError::Protocol {
                message: format!("invalid tool arguments: {error}"),
            })?
        };
        Ok(vec![ModelEvent::ToolCallCompleted {
            id: buffer.id,
            name: buffer.name,
            input,
        }])
    }

    /// The response is over, complete or cut short. A response that called a function always ends
    /// with `tool_calls`; otherwise `finish_without_tools` says why it stopped.
    fn on_response_finished(
        &mut self,
        value: &Value,
        finish_without_tools: &str,
    ) -> Vec<ModelEvent> {
        self.saw_terminal = true;
        let response = &value["response"];
        if let Some(model) = response["model"].as_str() {
            self.model = model.to_string();
        }
        let raw_usage = response
            .get("usage")
            .filter(|usage| !usage.is_null())
            .and_then(|usage| {
                serde_json::from_value::<RawOpenAiResponsesUsage>(usage.clone()).ok()
            });
        if let Some(raw) = raw_usage {
            self.usage = OpenAiResponsesUsageMapper::map_usage(&raw);
        }
        self.stop_reason = if self.saw_function_call {
            "tool_calls"
        } else {
            finish_without_tools
        }
        .to_string();

        let result = ModelResult {
            stop_reason: self.stop_reason.clone(),
            usage: self.usage.clone(),
            cost: OpenAiResponsesUsageMapper::calculate_cost(&self.usage, &self.model),
        };
        self.result = Some(result.clone());
        vec![
            ModelEvent::UsageUpdate {
                usage: self.usage.clone(),
            },
            ModelEvent::Completed { result },
        ]
    }

    fn on_response_failed(&mut self, value: &Value) -> ModelError {
        self.saw_terminal = true;
        let response = &value["response"];
        let message = response["error"]["message"]
            .as_str()
            .map(str::to_string)
            .or_else(|| {
                response["incomplete_details"]["reason"]
                    .as_str()
                    .map(|reason| format!("incomplete: {reason}"))
            })
            .unwrap_or_else(|| "Unknown error (no error details in response)".to_string());
        let code = response["error"]["code"]
            .as_str()
            .unwrap_or("response_failed")
            .to_string();
        ModelError::BackendError { message, code }
    }
}

/// The OpenAI API puts code and message at the top level; the ChatGPT subscription backend nests
/// them under `error`.
fn stream_error(value: &Value) -> ModelError {
    let field = |name: &str| {
        value
            .get(name)
            .or_else(|| value.get("error").and_then(|error| error.get(name)))
            .and_then(Value::as_str)
    };
    ModelError::BackendError {
        message: field("message")
            .unwrap_or("Responses stream error")
            .to_string(),
        code: field("code")
            .or_else(|| value.pointer("/error/type").and_then(Value::as_str))
            .unwrap_or("stream_error")
            .to_string(),
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

fn output_index(value: &Value) -> u64 {
    value["output_index"].as_u64().unwrap_or(0)
}

/// An event for the frame's non-empty `delta`, or none.
fn delta_event(value: &Value, make: impl FnOnce(String) -> ModelEvent) -> Vec<ModelEvent> {
    match value["delta"].as_str() {
        Some(delta) if !delta.is_empty() => vec![make(delta.to_string())],
        _ => vec![],
    }
}
