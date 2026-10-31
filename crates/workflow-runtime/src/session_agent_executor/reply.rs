//! Reading a step's final message as the output schema's value.

use rusty_workflow_core::StructuredOutputMode;
use serde_json::Value;

use super::IsolatedSessionAgentExecutor;
use crate::markdown_reply;
use crate::orchestration::{AgentExecutionError, AgentStepRequest};

impl IsolatedSessionAgentExecutor {
    /// Whether the backend is asked for schema-conforming JSON, or the schema travels in the
    /// prompt. `HostValidated` opts out of the native schema even when the backend advertises it,
    /// and replies are written as Markdown whenever the schema allows it, so the native JSON
    /// mode is not used for those either.
    pub(super) fn native_schema(
        &self,
        request: &AgentStepRequest,
    ) -> Result<bool, AgentExecutionError> {
        let markdown = markdown_reply::supports(&request.output_schema);
        let backend = &self.parent.default_backend;
        let native = backend.capabilities().structured_output
            && !markdown
            && !matches!(
                request.structured_output,
                StructuredOutputMode::HostValidated | StructuredOutputMode::Text
            );
        if !native && !markdown && request.structured_output == StructuredOutputMode::Require {
            return Err(AgentExecutionError::new(
                "unsupported_capability",
                format!(
                    "backend {} lacks structured output and step {} requires it \
                     (set structured_output to host_validated_fallback to allow JSON text)",
                    backend.descriptor().name,
                    request.node_id
                ),
            ));
        }
        Ok(native)
    }
}

/// Accept a bare JSON value, or one wrapped in a single code fence (models
/// without native structured output often add one despite instructions).
fn parse_json_reply(text: &str) -> Result<Value, serde_json::Error> {
    let trimmed = text.trim();
    serde_json::from_str(trimmed).or_else(|error| {
        let inner = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .and_then(|rest| rest.strip_suffix("```"));
        match inner {
            Some(inner) => serde_json::from_str(inner.trim()),
            None => Err(error),
        }
    })
}

/// A step's final message as the schema's value: Markdown in the layout from
/// `markdown_reply`, or (accepted for leniency) JSON.
pub(super) fn parse_reply(schema: &Value, text: &str) -> Result<Value, String> {
    match parse_json_reply(text) {
        Ok(value) => Ok(value),
        Err(_) if markdown_reply::supports(schema) => markdown_reply::parse(schema, text),
        Err(error) => Err(format!("final agent message was not valid JSON: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_replies_may_be_fenced() {
        assert_eq!(
            parse_json_reply(" {\"a\":1} ").unwrap(),
            serde_json::json!({"a": 1})
        );
        assert_eq!(
            parse_json_reply("```json\n{\"a\":1}\n```").unwrap(),
            serde_json::json!({"a": 1})
        );
        assert!(parse_json_reply("Here you go: {\"a\":1}").is_err());
    }
}
