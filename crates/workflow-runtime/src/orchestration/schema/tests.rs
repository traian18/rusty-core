use serde_json::json;

use super::{BasicSchemaValidator, SchemaValidator};

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
