//! Reading a Markdown reply back into the schema's value, and repairing what is almost right.

use crate::markdown_reply::layout::normalize;
use crate::markdown_reply::object::{parse_object, promote_bare_names};
use crate::markdown_reply::schema::{enum_values, kind, properties, Kind, BODY_FIELDS};
use serde_json::{Map, Value};

pub(crate) fn parse(schema: &Value, text: &str) -> Result<Value, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("the reply was empty".into());
    }
    // A bare `status` / `summary` line counts as a section heading.
    let mut value = parse_object(schema, &promote_bare_names(schema, text));
    if let Value::Object(map) = &mut value {
        fill_missing(schema, map, text);
    }
    repair(schema, &mut value);
    Ok(value)
}

/// Models do not always format a reply the same way, and the work is done
/// either way: what the reply does not state is inferred from its text rather
/// than failing the step on form.
pub(super) fn fill_missing(schema: &Value, map: &mut Map<String, Value>, text: &str) {
    for (name, property) in properties(schema) {
        if map.contains_key(&name) || !is_required(schema, &name) {
            continue;
        }
        let filled = match kind(property) {
            Kind::List | Kind::Items => Some(Value::Array(Vec::new())),
            Kind::Scalar => {
                let values = enum_values(property);
                if !values.is_empty() {
                    // The reply names its status somewhere, else it is the
                    // success value (the first one listed).
                    let found = text
                        .lines()
                        .map(|line| normalize(line.trim_start_matches(['-', '*', ' '])))
                        .find_map(|line| {
                            let line = line.strip_prefix(&format!("{name}_")).unwrap_or(&line);
                            values.iter().find(|value| normalize(value) == line)
                        });
                    values
                        .first()
                        .map(|first| Value::String(found.unwrap_or(first).clone()))
                } else if BODY_FIELDS.contains(&name.as_str()) {
                    Some(Value::String(text.to_owned()))
                } else if name == "verdict" {
                    // Stated by the check results when the reply does not.
                    let results = map.get("criteria").and_then(Value::as_array);
                    let passing = results.is_some_and(|items| {
                        !items.is_empty() && items.iter().all(|item| item["status"] == "pass")
                    });
                    Some(Value::String(if passing {
                        "pass".into()
                    } else {
                        "fail: the reply did not report every criterion as passing".into()
                    }))
                } else {
                    None
                }
            }
            Kind::Object => None,
        };
        if let Some(filled) = filled {
            map.insert(name, filled);
        }
    }
}

/// An empty text a schema calls non-empty is not worth failing a step for:
/// it takes the item's ID (or a plain marker) instead.
pub(super) fn repair(schema: &Value, value: &mut Value) {
    match value {
        Value::String(text) => {
            // A status the schema does not list (`Done`) is read as the
            // success value, as a missing one is.
            let values = enum_values(schema);
            if !values.is_empty() && !values.contains(text) {
                // An unclear check result is "unverified", never a pass.
                *text = if values.iter().any(|value| value == "unverified") {
                    "unverified".into()
                } else {
                    values[0].clone()
                };
            }
            if text.trim().is_empty()
                && schema.get("minLength").and_then(Value::as_u64).unwrap_or(0) > 0
            {
                *text = "(not stated)".into();
            }
        }
        Value::Array(items) => {
            if let Some(item_schema) = schema.get("items") {
                for item in items {
                    repair(item_schema, item);
                }
            }
        }
        Value::Object(map) => {
            let id = map.get("id").and_then(Value::as_str).map(str::to_owned);
            for (name, property) in properties(schema) {
                if let Some(field) = map.get_mut(&name) {
                    if let (Value::String(text), Some(id)) = (&mut *field, &id) {
                        if text.trim().is_empty()
                            && property
                                .get("minLength")
                                .and_then(Value::as_u64)
                                .unwrap_or(0)
                                > 0
                        {
                            *text = id.clone();
                        }
                    }
                    repair(property, field);
                }
            }
        }
        _ => {}
    }
}

pub(super) fn is_required(schema: &Value, name: &str) -> bool {
    schema
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|required| required.iter().any(|item| item.as_str() == Some(name)))
}
