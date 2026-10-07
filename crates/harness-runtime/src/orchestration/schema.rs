//! Schema resolution and host-side JSON Schema validation.
//!
//! Provider-side structured output is helpful but never authoritative: every
//! structured boundary is validated here. The validator implements exactly
//! [`SUPPORTED_SCHEMA_KEYWORDS`]; the compiler rejects any other keyword, so
//! no schema is ever partially interpreted.

use std::collections::BTreeMap;

use harness_core::orchestration::{
    task_plan_schema, SchemaReference, SUPPORTED_SCHEMA_KEYWORDS, TASK_PLAN_SCHEMA_ID,
    TASK_PLAN_SCHEMA_REVISION,
};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct SchemaResolutionError {
    pub message: String,
}

pub trait SchemaResolver: Send + Sync {
    fn resolve(&self, reference: &SchemaReference) -> Result<Value, SchemaResolutionError>;
}

/// Resolves inline schemas directly and registry schemas from an in-memory
/// `(schema_id, revision)` table, which always holds the built-in schemas
/// (the task plan, [`TASK_PLAN_SCHEMA_ID`]).
#[derive(Debug, Clone)]
pub struct InMemorySchemaResolver {
    schemas: BTreeMap<(String, u64), Value>,
}

impl Default for InMemorySchemaResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemorySchemaResolver {
    pub fn new() -> Self {
        let mut schemas = BTreeMap::new();
        schemas.insert(
            (TASK_PLAN_SCHEMA_ID.to_owned(), TASK_PLAN_SCHEMA_REVISION),
            task_plan_schema(),
        );
        Self { schemas }
    }

    pub fn insert(&mut self, schema_id: impl Into<String>, revision: u64, schema: Value) {
        self.schemas.insert((schema_id.into(), revision), schema);
    }

    pub fn with_schema(
        mut self,
        schema_id: impl Into<String>,
        revision: u64,
        schema: Value,
    ) -> Self {
        self.insert(schema_id, revision, schema);
        self
    }
}

impl SchemaResolver for InMemorySchemaResolver {
    fn resolve(&self, reference: &SchemaReference) -> Result<Value, SchemaResolutionError> {
        match reference {
            SchemaReference::Inline { schema, .. } => Ok(schema.clone()),
            SchemaReference::Registry {
                schema_id,
                revision,
            } => self
                .schemas
                .get(&(schema_id.clone(), *revision))
                .cloned()
                .ok_or_else(|| SchemaResolutionError {
                    message: format!("schema {schema_id}@{revision} is not registered"),
                }),
        }
    }
}

/// All violations found in one value, so a model retry receives the full
/// list rather than fixing one issue per attempt.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{}", .issues.join("; "))]
pub struct SchemaValidationError {
    pub issues: Vec<String>,
}

pub trait SchemaValidator: Send + Sync {
    fn validate(&self, schema: &Value, value: &Value) -> Result<(), SchemaValidationError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BasicSchemaValidator;

impl SchemaValidator for BasicSchemaValidator {
    fn validate(&self, schema: &Value, value: &Value) -> Result<(), SchemaValidationError> {
        let mut issues = Vec::new();
        validate(schema, value, "$", &mut issues);
        if issues.is_empty() {
            Ok(())
        } else {
            Err(SchemaValidationError { issues })
        }
    }
}

fn validate(schema: &Value, value: &Value, path: &str, issues: &mut Vec<String>) {
    let mut report = |message: String| issues.push(format!("{path}: {message}"));
    let Some(object) = schema.as_object() else {
        report("schema must be an object".into());
        return;
    };
    if let Some(keyword) = object
        .keys()
        .find(|key| !SUPPORTED_SCHEMA_KEYWORDS.contains(&key.as_str()))
    {
        report(format!("unsupported schema keyword {keyword}"));
        return;
    }
    if let Some(expected) = object.get("const") {
        if value != expected {
            report(format!("must equal {expected}"));
        }
    }
    if let Some(allowed) = object.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            report(format!(
                "{value} is not one of {}",
                Value::Array(allowed.clone())
            ));
        }
    }
    if let Some(kind) = object.get("type").and_then(Value::as_str) {
        let matches = match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => value.is_i64() || value.is_u64(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            other => {
                report(format!("unsupported schema type {other}"));
                return;
            }
        };
        if !matches {
            report(format!("expected {kind}, got {}", type_name(value)));
            // Nested constraints are meaningless against the wrong type.
            return;
        }
    }

    if let Some(text) = value.as_str() {
        let length = text.chars().count() as u64;
        if let Some(minimum) = object.get("minLength").and_then(Value::as_u64) {
            if length < minimum {
                report(format!("length {length} is below minLength {minimum}"));
            }
        }
        if let Some(maximum) = object.get("maxLength").and_then(Value::as_u64) {
            if length > maximum {
                report(format!("length {length} exceeds maxLength {maximum}"));
            }
        }
    }
    if let Some(number) = value.as_f64() {
        if let Some(minimum) = object.get("minimum").and_then(Value::as_f64) {
            if number < minimum {
                report(format!("{number} is below minimum {minimum}"));
            }
        }
        if let Some(maximum) = object.get("maximum").and_then(Value::as_f64) {
            if number > maximum {
                report(format!("{number} exceeds maximum {maximum}"));
            }
        }
    }
    if let Some(items) = value.as_array() {
        let count = items.len() as u64;
        if let Some(minimum) = object.get("minItems").and_then(Value::as_u64) {
            if count < minimum {
                report(format!("{count} items is below minItems {minimum}"));
            }
        }
        if let Some(maximum) = object.get("maxItems").and_then(Value::as_u64) {
            if count > maximum {
                report(format!("{count} items exceeds maxItems {maximum}"));
            }
        }
        if let Some(item_schema) = object.get("items") {
            for (index, child) in items.iter().enumerate() {
                validate(item_schema, child, &format!("{path}/{index}"), issues);
            }
        }
    }
    if let Some(fields) = value.as_object() {
        let properties = object.get("properties").and_then(Value::as_object);
        for key in object
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !fields.contains_key(key) {
                issues.push(format!("{path}: required property {key} is missing"));
            }
        }
        for (key, child) in fields {
            match properties.and_then(|properties| properties.get(key)) {
                Some(child_schema) => {
                    validate(child_schema, child, &format!("{path}/{key}"), issues)
                }
                None => match object.get("additionalProperties") {
                    Some(Value::Bool(false)) => {
                        issues.push(format!("{path}: additional property {key} is not allowed"))
                    }
                    Some(additional @ Value::Object(_)) => {
                        validate(additional, child, &format!("{path}/{key}"), issues)
                    }
                    _ => {}
                },
            }
        }
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reports_every_violation_at_once() {
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["a", "b"],
            "properties": {
                "a": { "type": "string", "minLength": 2 },
                "b": { "type": "integer", "minimum": 0 }
            }
        });
        let error = BasicSchemaValidator
            .validate(&schema, &json!({"a": "x", "c": 1}))
            .expect_err("invalid");
        assert_eq!(error.issues.len(), 3, "{error}");
    }

    #[test]
    fn accepts_conforming_values_and_rejects_unknown_keywords() {
        let schema = json!({"type": "array", "items": {"enum": [1, 2]}, "maxItems": 2});
        BasicSchemaValidator
            .validate(&schema, &json!([1, 2]))
            .expect("valid");
        assert!(BasicSchemaValidator
            .validate(&json!({"pattern": "x"}), &json!("x"))
            .is_err());
    }
}
