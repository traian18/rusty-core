//! The result a builder or a reviewer must return, and when a review accepts a task.

#[cfg(test)]
mod tests;

use serde_json::{json, Value};

use crate::orchestration::{AgentExecutionError, BasicSchemaValidator, SchemaValidator};
use crate::task_queue::model::Task;

/// The schema of a builder's result; a reviewer's also lists per-criterion evidence.
pub(in crate::task_queue) fn result_schema(review: bool) -> Value {
    let mut schema = json!({"type":"object","additionalProperties":false,"required":["status","summary"],"properties":{
        "status":{"type":"string","enum":["complete","needs_repair","blocked_environment","blocked_user","checkpoint"]},"summary":{"type":"string","minLength":1},
        // Carried to the later tasks' planners and builders (known_locations), so they do not search again.
        "locations":{"type":"array","maxItems":12,"description":"Where the code this task is about lives, for the tasks after it: one line each, such as \"ContextPolicy::reading: rusty-core/crates/rusty-context/src/policy.rs:128-150\".","items":{"type":"string","maxLength":240}}
    }});
    if review {
        add_criteria_evidence(&mut schema);
    }
    schema
}

fn add_criteria_evidence(schema: &mut Value) {
    if let Some(required) = schema["required"].as_array_mut() {
        required.push(json!("criteria"));
    }
    schema["properties"]["criteria"] = json!({"type":"array","items":{"type":"object","additionalProperties":false,"required":["id","evidence"],"properties":{"id":{"type":"string"},"evidence":{"type":"string","minLength":1}}}});
}

/// Whether a review found every criterion of `task` met, each with evidence.
pub(in crate::task_queue) fn review_complete(task: &Task, result: &Value) -> bool {
    if result["status"] != "complete" {
        return false;
    }
    let Some(items) = result["criteria"].as_array() else {
        return false;
    };
    let expected: Vec<&str> = task.criterion_ids.iter().map(String::as_str).collect();
    crate::orchestration::steps::match_results(&expected, items, |item| item["id"].as_str())
        .iter()
        .all(|found| {
            found.is_some_and(|item| {
                item["evidence"]
                    .as_str()
                    .is_some_and(|text| !text.trim().is_empty())
            })
        })
}

/// Checks a builder's or reviewer's reply against `schema`; `code` names what was malformed.
pub(in crate::task_queue) fn validate(
    schema: &Value,
    reply: &Value,
    code: &'static str,
) -> Result<(), AgentExecutionError> {
    BasicSchemaValidator
        .validate(schema, reply)
        .map_err(|error| AgentExecutionError::new(code, error.to_string()))
}
