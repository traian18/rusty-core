//! Fitting free text to a scalar the schema expects (an enum word, a number, a flag).

use crate::markdown_reply::layout::normalize;
use crate::markdown_reply::schema::enum_values;
use serde_json::Value;

pub(super) fn coerce(schema: &Value, text: &str) -> Value {
    match schema.get("type").and_then(Value::as_str) {
        Some("integer") | Some("number") => {
            let number = text.trim().trim_matches('`');
            if let Ok(value) = number.parse::<i64>() {
                return Value::from(value);
            }
            if let Ok(value) = number.parse::<f64>() {
                return Value::from(value);
            }
        }
        Some("boolean") => {
            return Value::Bool(matches!(
                normalize(text).as_str(),
                "true" | "yes" | "y" | "1"
            ))
        }
        _ => {}
    }
    let values = enum_values(schema);
    if !values.is_empty() {
        let plain = normalize(text);
        if let Some(found) = values.iter().find(|value| normalize(value) == plain) {
            return Value::String(found.clone());
        }
        let first = plain
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .find(|word| !word.is_empty())
            .unwrap_or("");
        if let Some(found) = values.iter().find(|value| normalize(value) == first) {
            return Value::String(found.clone());
        }
        // `failed`, `passed`, `completed`.
        if let Some(found) = values
            .iter()
            .find(|value| !first.is_empty() && first.starts_with(&normalize(value)))
        {
            return Value::String(found.clone());
        }
    }
    Value::String(text.to_owned())
}
