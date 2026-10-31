//! Checking a value against a schema, reporting every violation at once.
//!
//! The checks run in a fixed order (`const`, `enum`, `type`, then the keywords of the value's own
//! kind), so the issues of a value always read the same.

use rusty_workflow_core::SUPPORTED_SCHEMA_KEYWORDS;
use serde_json::{Map, Value};
use thiserror::Error;

/// All violations found in one value, so a model retry receives the full list rather than fixing
/// one issue per attempt.
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
    let Some(rules) = schema.as_object() else {
        issues.push(format!("{path}: schema must be an object"));
        return;
    };
    let mut check = Check {
        rules,
        value,
        path,
        issues,
    };
    if check.keywords_are_supported() && check.general() {
        check.text();
        check.number();
        check.array();
        check.object();
    }
}

/// One value against the rules of one schema object.
struct Check<'a, 'i> {
    rules: &'a Map<String, Value>,
    value: &'a Value,
    path: &'a str,
    issues: &'i mut Vec<String>,
}

impl<'a> Check<'a, '_> {
    fn report(&mut self, message: String) {
        self.issues.push(format!("{}: {message}", self.path));
    }

    fn rule(&self, keyword: &str) -> Option<&'a Value> {
        self.rules.get(keyword)
    }

    fn keywords_are_supported(&mut self) -> bool {
        let unsupported = self
            .rules
            .keys()
            .find(|key| !SUPPORTED_SCHEMA_KEYWORDS.contains(&key.as_str()));
        match unsupported {
            Some(keyword) => {
                self.report(format!("unsupported schema keyword {keyword}"));
                false
            }
            None => true,
        }
    }

    /// `const`, `enum` and `type`. False when nested constraints are meaningless: the schema's
    /// type is unknown or the value has another type.
    fn general(&mut self) -> bool {
        if let Some(expected) = self
            .rule("const")
            .filter(|expected| *expected != self.value)
        {
            let message = format!("must equal {expected}");
            self.report(message);
        }
        if let Some(allowed) = self.rule("enum").and_then(Value::as_array) {
            if !allowed.contains(self.value) {
                let message = format!(
                    "{} is not one of {}",
                    self.value,
                    Value::Array(allowed.clone())
                );
                self.report(message);
            }
        }
        match self.rule("type").and_then(Value::as_str) {
            Some(kind) => self.type_matches(kind),
            None => true,
        }
    }

    fn type_matches(&mut self, kind: &str) -> bool {
        let value = self.value;
        let matches = match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => value.is_i64() || value.is_u64(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            other => {
                self.report(format!("unsupported schema type {other}"));
                return false;
            }
        };
        if !matches {
            self.report(format!("expected {kind}, got {}", type_name(value)));
        }
        matches
    }

    fn bound_u64(&self, keyword: &str) -> Option<u64> {
        self.rule(keyword).and_then(Value::as_u64)
    }

    fn bound_f64(&self, keyword: &str) -> Option<f64> {
        self.rule(keyword).and_then(Value::as_f64)
    }

    fn text(&mut self) {
        let Some(text) = self.value.as_str() else {
            return;
        };
        let length = text.chars().count() as u64;
        if let Some(minimum) = self.bound_u64("minLength").filter(|min| length < *min) {
            self.report(format!("length {length} is below minLength {minimum}"));
        }
        if let Some(maximum) = self.bound_u64("maxLength").filter(|max| length > *max) {
            self.report(format!("length {length} exceeds maxLength {maximum}"));
        }
    }

    fn number(&mut self) {
        let Some(number) = self.value.as_f64() else {
            return;
        };
        if let Some(minimum) = self.bound_f64("minimum").filter(|min| number < *min) {
            self.report(format!("{number} is below minimum {minimum}"));
        }
        if let Some(maximum) = self.bound_f64("maximum").filter(|max| number > *max) {
            self.report(format!("{number} exceeds maximum {maximum}"));
        }
    }

    fn array(&mut self) {
        let Some(items) = self.value.as_array() else {
            return;
        };
        let count = items.len() as u64;
        if let Some(minimum) = self.bound_u64("minItems").filter(|min| count < *min) {
            self.report(format!("{count} items is below minItems {minimum}"));
        }
        if let Some(maximum) = self.bound_u64("maxItems").filter(|max| count > *max) {
            self.report(format!("{count} items exceeds maxItems {maximum}"));
        }
        if let Some(item_schema) = self.rule("items") {
            for (index, child) in items.iter().enumerate() {
                let path = format!("{}/{index}", self.path);
                validate(item_schema, child, &path, self.issues);
            }
        }
    }

    fn object(&mut self) {
        let Some(fields) = self.value.as_object() else {
            return;
        };
        self.required_properties(fields);
        let properties = self.rule("properties").and_then(Value::as_object);
        for (key, child) in fields {
            let path = format!("{}/{key}", self.path);
            match properties.and_then(|properties| properties.get(key)) {
                Some(child_schema) => validate(child_schema, child, &path, self.issues),
                None => self.additional_property(key, child, &path),
            }
        }
    }

    fn required_properties(&mut self, fields: &Map<String, Value>) {
        let required = self
            .rule("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);
        let missing: Vec<&str> = required.filter(|key| !fields.contains_key(*key)).collect();
        for key in missing {
            self.report(format!("required property {key} is missing"));
        }
    }

    fn additional_property(&mut self, key: &str, child: &Value, path: &str) {
        match self.rule("additionalProperties") {
            Some(Value::Bool(false)) => {
                self.report(format!("additional property {key} is not allowed"));
            }
            Some(additional @ Value::Object(_)) => validate(additional, child, path, self.issues),
            _ => {}
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
