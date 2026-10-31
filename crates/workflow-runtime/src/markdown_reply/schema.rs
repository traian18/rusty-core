//! What a step's output schema asks for: which properties exist, which are text, lists or pairs.

use serde_json::Value;

pub(super) const BODY_FIELDS: [&str; 9] = [
    "text",
    "instructions",
    "evidence",
    "summary",
    "description",
    "content",
    "message",
    "finding",
    "detail",
];

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Kind {
    Scalar,
    List,
    Items,
    Object,
}

pub(super) fn kind(schema: &Value) -> Kind {
    match schema.get("type").and_then(Value::as_str) {
        Some("array") => match schema.get("items").map(kind) {
            Some(Kind::Scalar) | None => Kind::List,
            _ => Kind::Items,
        },
        Some("object") => Kind::Object,
        Some(_) => Kind::Scalar,
        None if schema.get("properties").is_some() => Kind::Object,
        None if schema.get("items").is_some() => Kind::Items,
        None => Kind::Scalar,
    }
}

/// Whether a reply for this schema can be written as Markdown.
pub(crate) fn supports(schema: &Value) -> bool {
    kind(schema) == Kind::Object
        && schema
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| !properties.is_empty())
}

pub(super) fn properties(schema: &Value) -> Vec<(String, &Value)> {
    let Some(map) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut ordered = Vec::new();
    for name in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if let Some(value) = map.get(name) {
            ordered.push((name.to_owned(), value));
        }
    }
    for (name, value) in map {
        if !ordered.iter().any(|(known, _)| known == name) {
            ordered.push((name.clone(), value));
        }
    }
    ordered
}

pub(super) fn enum_values(schema: &Value) -> Vec<String> {
    schema
        .get("enum")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn hint(schema: &Value) -> String {
    let values = enum_values(schema);
    if values.is_empty() {
        match schema.get("type").and_then(Value::as_str) {
            Some("integer") | Some("number") => "<number>".into(),
            Some("boolean") => "<true or false>".into(),
            // An optional field says so, so it is not mistaken for the body.
            _ => match schema.get("description").and_then(Value::as_str) {
                Some(d) if d.starts_with("Optional") => {
                    format!(
                        "<{}; a short name only, leave this line out if none>",
                        d.trim_end_matches('.')
                    )
                }
                _ => "<text>".into(),
            },
        }
    } else {
        values.join(" | ")
    }
}

/// The property that carries an item's main text.
pub(super) fn body_field(schema: &Value) -> Option<String> {
    let props = properties(schema);
    for wanted in BODY_FIELDS {
        if props.iter().any(|(name, value)| {
            name == wanted && kind(value) == Kind::Scalar && enum_values(value).is_empty()
        }) {
            return Some(wanted.to_owned());
        }
    }
    None
}

/// `{id, text}`: written as one bullet per item.
pub(super) fn is_pair(schema: &Value) -> bool {
    let props = properties(schema);
    props.len() == 2
        && props.iter().any(|(name, _)| name == "id")
        && body_field(schema).is_some_and(|body| props.iter().any(|(name, _)| *name == body))
        && props.iter().all(|(_, value)| kind(value) == Kind::Scalar)
}
