//! Checks on the JSON Schemas a definition carries: inline schemas may use only the keywords the
//! host validator implements.

use serde_json::{Map, Value};

use super::{Issues, SUPPORTED_SCHEMA_KEYWORDS};
use crate::definition::SchemaReference;

const PRIMITIVE_TYPES: [&str; 7] = [
    "null", "boolean", "object", "array", "number", "integer", "string",
];

pub(super) fn check_schema_reference(reference: &SchemaReference, path: &str, issues: &mut Issues) {
    match reference {
        SchemaReference::Inline { name, schema } => {
            if name.trim().is_empty() {
                issues.add(path, "empty_schema_name", "schema name cannot be empty");
            }
            check_schema_value(schema, path, issues);
        }
        SchemaReference::Registry {
            schema_id,
            revision,
        } => {
            if schema_id.trim().is_empty() || *revision == 0 {
                issues.add(
                    path,
                    "invalid_schema_reference",
                    "registry schema requires a non-empty id and positive revision",
                );
            }
        }
    }
}

fn check_schema_value(schema: &Value, path: &str, issues: &mut Issues) {
    let Some(object) = schema.as_object() else {
        issues.add(
            path,
            "invalid_schema",
            "inline schema must be a JSON object",
        );
        return;
    };
    check_keywords(object, path, issues);
    check_type(object, path, issues);
    check_required(object, path, issues);
    check_properties(object, path, issues);
    check_items(object, path, issues);
    check_additional_properties(object, path, issues);
    check_enum(object, path, issues);
}

fn check_keywords(object: &Map<String, Value>, path: &str, issues: &mut Issues) {
    for keyword in object.keys() {
        if !SUPPORTED_SCHEMA_KEYWORDS.contains(&keyword.as_str()) {
            issues.add(
                format!("{path}.{keyword}"),
                "unsupported_schema_keyword",
                format!("schema keyword {keyword} is not supported by the host validator"),
            );
        }
    }
}

fn check_type(object: &Map<String, Value>, path: &str, issues: &mut Issues) {
    let Some(schema_type) = object.get("type") else {
        return;
    };
    let valid = schema_type
        .as_str()
        .is_some_and(|value| PRIMITIVE_TYPES.contains(&value));
    if !valid {
        issues.add(
            format!("{path}.type"),
            "invalid_schema",
            "type must be a supported JSON Schema primitive",
        );
    }
}

fn check_required(object: &Map<String, Value>, path: &str, issues: &mut Issues) {
    let Some(required) = object.get("required") else {
        return;
    };
    let valid = required
        .as_array()
        .is_some_and(|values| values.iter().all(Value::is_string));
    if !valid {
        issues.add(
            format!("{path}.required"),
            "invalid_schema",
            "required must be an array of strings",
        );
    }
}

fn check_properties(object: &Map<String, Value>, path: &str, issues: &mut Issues) {
    let Some(properties) = object.get("properties") else {
        return;
    };
    match properties.as_object() {
        Some(properties) => {
            for (name, child) in properties {
                check_schema_value(child, &format!("{path}.properties.{name}"), issues);
            }
        }
        None => issues.add(
            format!("{path}.properties"),
            "invalid_schema",
            "properties must be an object",
        ),
    }
}

fn check_items(object: &Map<String, Value>, path: &str, issues: &mut Issues) {
    if let Some(items) = object.get("items") {
        check_schema_value(items, &format!("{path}.items"), issues);
    }
}

fn check_additional_properties(object: &Map<String, Value>, path: &str, issues: &mut Issues) {
    let Some(additional) = object.get("additionalProperties") else {
        return;
    };
    if !additional.is_boolean() && !additional.is_object() {
        issues.add(
            format!("{path}.additionalProperties"),
            "invalid_schema",
            "additionalProperties must be a boolean or schema object",
        );
    }
}

fn check_enum(object: &Map<String, Value>, path: &str, issues: &mut Issues) {
    let Some(values) = object.get("enum") else {
        return;
    };
    if values.as_array().is_none_or(|values| values.is_empty()) {
        issues.add(
            format!("{path}.enum"),
            "invalid_schema",
            "enum must be a non-empty array",
        );
    }
}
