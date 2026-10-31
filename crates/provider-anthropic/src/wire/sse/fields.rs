//! Reading values out of the JSON an event carries.

use serde_json::Value;

pub(super) fn block_index(value: &Value) -> usize {
    value["index"].as_u64().unwrap_or_default() as usize
}

pub(super) fn text_of(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

/// A tool block's `input` as it arrived, when it already carries something.
pub(super) fn initial_input(content: &Value) -> Option<String> {
    content
        .get("input")
        .filter(|input| {
            !input.is_null() && input.as_object().is_none_or(|object| !object.is_empty())
        })
        .map(ToString::to_string)
}
