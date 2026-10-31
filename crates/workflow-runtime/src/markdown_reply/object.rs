//! Reading the sections of an object-shaped reply.

use crate::markdown_reply::coerce::coerce;
use crate::markdown_reply::items::{bullets, parse_items};
use crate::markdown_reply::layout::{headings, normalize, sections};
use crate::markdown_reply::schema::{enum_values, kind, properties, Kind};
use serde_json::{Map, Value};

/// `status` / `**Summary:**` alone on a line becomes `## status`; a short
/// `status: complete` line for an enum property too.
pub(super) fn promote_bare_names(schema: &Value, text: &str) -> String {
    let props = properties(schema);
    let mut fenced = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        let key = normalize(line);
        let plain = !line.trim_start().starts_with('#');
        if !fenced && plain && props.iter().any(|(name, _)| *name == key) {
            out.push(format!("## {key}"));
            continue;
        }
        if !fenced && plain {
            if let Some((name, value)) = line.split_once(':') {
                let name = normalize(name.trim_start_matches(['-', '#', ' ']));
                let inline = props.iter().any(|(prop, property)| {
                    *prop == name
                        && kind(property) == Kind::Scalar
                        && !enum_values(property).is_empty()
                }) && !value.trim().is_empty();
                if inline {
                    out.push(format!("## {name}\n{}", value.trim()));
                    continue;
                }
            }
        }
        out.push(line.to_owned());
    }
    out.join("\n")
}

pub(super) fn parse_object(schema: &Value, text: &str) -> Value {
    let props = properties(schema);
    let matching = |title: &str| props.iter().any(|(name, _)| *name == normalize(title));
    // Models do not always honour the exact level; use the shallowest heading
    // that names one of the expected properties.
    let level = headings(text)
        .into_iter()
        .filter(|(_, _, title)| matching(title))
        .map(|(_, level, _)| level)
        .min();
    let mut out = Map::new();
    let Some(level) = level else {
        return Value::Object(out);
    };
    for (title, body) in sections(text, level) {
        let key = normalize(&title);
        if let Some((name, property)) = props.iter().find(|(name, _)| *name == key) {
            if let Some(value) = parse_value(name, property, &body, level) {
                out.insert(name.clone(), value);
            }
        }
    }
    Value::Object(out)
}

pub(super) fn parse_value(name: &str, schema: &Value, body: &str, level: usize) -> Option<Value> {
    match kind(schema) {
        Kind::Scalar => Some(coerce(schema, body.trim())),
        Kind::List => Some(Value::Array(
            bullets(body)
                .into_iter()
                .map(|item| coerce(schema.get("items").unwrap_or(&Value::Null), &item))
                .collect(),
        )),
        Kind::Items => {
            let items = schema.get("items")?;
            Some(Value::Array(parse_items(name, items, body, level)))
        }
        Kind::Object => {
            let value = parse_object(schema, body);
            value
                .as_object()
                .is_some_and(|map| !map.is_empty())
                .then_some(value)
        }
    }
}
