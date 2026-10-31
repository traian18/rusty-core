//! Writing a workflow value as Markdown, for the prompt's `workflow_input`.

use crate::markdown_reply::layout::heading;
use serde_json::{Map, Value};

/// A step's input as Markdown: one `## name` section per field, text as it
/// is (never escaped), lists as bullets, records as `### ID` blocks. This is
/// how earlier steps' results reach the next prompt.
pub(crate) fn render_input(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Object(fields) => {
            let mut out = String::new();
            render_fields(fields, 2, &mut out);
            out.trim_end().to_owned()
        }
        other => render_inline(other),
    }
}

pub(super) fn render_inline(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        Value::Array(items) => items
            .iter()
            .map(render_inline)
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

pub(super) fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Object(_) | Value::Array(_))
}

pub(super) fn render_fields(fields: &Map<String, Value>, level: usize, out: &mut String) {
    for (name, value) in fields {
        if value.is_null() {
            continue;
        }
        out.push_str(&heading(level, name));
        out.push('\n');
        render_body(value, level, out);
        out.push_str("\n\n");
    }
}

pub(super) fn render_body(value: &Value, level: usize, out: &mut String) {
    match value {
        Value::Array(items) if items.iter().all(is_scalar) => {
            for item in items {
                out.push_str(&format!("- {}\n", render_inline(item)));
            }
        }
        Value::Array(items)
            if items.iter().all(|item| {
                item.as_object().is_some_and(|record| {
                    record.len() == 2
                        && record.contains_key("id")
                        && record.values().all(|value| value.is_string())
                })
            }) =>
        {
            // `{id, text}` pairs read best as `- ID: text`.
            for item in items {
                let record = item.as_object().unwrap();
                let text = record
                    .iter()
                    .find(|(name, _)| *name != "id")
                    .map_or("", |(_, value)| value.as_str().unwrap_or(""));
                out.push_str(&format!("- {}: {text}\n", render_inline(&record["id"])));
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                match item {
                    Value::Object(record) => {
                        let title = record
                            .get("id")
                            .or_else(|| record.get("task_id"))
                            .map_or_else(|| (index + 1).to_string(), render_inline);
                        out.push_str(&heading(level + 1, &title));
                        out.push('\n');
                        render_record(record, level + 1, out);
                    }
                    other => out.push_str(&format!("- {}\n", render_inline(other))),
                }
            }
        }
        Value::Object(fields) => render_fields(fields, level + 1, out),
        other => out.push_str(&render_inline(other)),
    }
}

/// Short fields first as `key: value`, then text and nested data under
/// deeper headings.
pub(super) fn render_record(record: &Map<String, Value>, level: usize, out: &mut String) {
    let short = |value: &Value| match value {
        Value::String(text) => !text.contains('\n') && text.len() <= 80,
        Value::Array(items) => items.iter().all(is_scalar),
        Value::Null => false,
        _ => true,
    };
    for (name, value) in record {
        if name != "id" && name != "task_id" && short(value) && !value.is_null() {
            out.push_str(&format!("{name}: {}\n", render_inline(value)));
        }
    }
    for (name, value) in record {
        if name == "id" || name == "task_id" || short(value) || value.is_null() {
            continue;
        }
        out.push_str(&heading(level + 1, name));
        out.push('\n');
        render_body(value, level + 1, out);
        out.push('\n');
    }
}

// ---------------------------------------------------------------- template
