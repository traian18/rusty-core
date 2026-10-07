//! The plan a task queue executes: requirements with observable acceptance
//! criteria, and ordered tasks that each cover some of them. Registered with
//! every schema registry as [`TASK_PLAN_SCHEMA_ID`], so a plan step can name
//! it instead of repeating it inline.

use serde_json::{json, Value};

pub const TASK_PLAN_SCHEMA_ID: &str = "rusty.task_plan";
pub const TASK_PLAN_SCHEMA_REVISION: u64 = 1;

/// The JSON Schema of a task plan. It matches what the task queue reads:
/// ids may be left blank or loose (the queue numbers and matches them), but
/// every requirement needs criteria and every task needs instructions.
pub fn task_plan_schema() -> Value {
    let ids = json!({ "type": "array", "items": { "type": "string" } });
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["status", "summary", "requirements", "tasks"],
        "properties": {
            "status": {
                "type": "string",
                "enum": ["ready"],
                "description": "ready once the plan can be executed"
            },
            "summary": {
                "type": "string",
                "minLength": 1,
                "description": "The plan in a few sentences, plus follow-ups only someone outside the workspace can do"
            },
            "requirements": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "text", "criteria"],
                    "properties": {
                        "id": { "type": "string" },
                        "text": { "type": "string", "minLength": 1 },
                        "criteria": {
                            "type": "array",
                            "minItems": 1,
                            "description": "Observable acceptance criteria",
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["id", "text"],
                                "properties": {
                                    "id": { "type": "string" },
                                    "text": { "type": "string", "minLength": 1 }
                                }
                            }
                        }
                    }
                }
            },
            "tasks": {
                "type": "array",
                "minItems": 1,
                "maxItems": 128,
                "description": "Ordered tasks; each changes the workspace",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "instructions", "requirement_ids", "criterion_ids", "depends_on"],
                    "properties": {
                        "id": { "type": "string" },
                        "instructions": { "type": "string", "minLength": 1 },
                        "requirement_ids": ids,
                        "criterion_ids": ids,
                        "depends_on": ids,
                        "flow": {
                            "type": "string",
                            "description": "Optional: the name of a flow to run for this task instead of building it directly"
                        }
                    }
                }
            }
        }
    })
}

/// Whether `schema` describes a task plan: the registered one, or an inline
/// object schema with requirements and tasks.
pub fn is_task_plan_schema(reference: &super::SchemaReference) -> bool {
    match reference {
        super::SchemaReference::Registry { schema_id, .. } => schema_id == TASK_PLAN_SCHEMA_ID,
        super::SchemaReference::Inline { schema, .. } => {
            let properties = &schema["properties"];
            properties.get("requirements").is_some() && properties.get("tasks").is_some()
        }
    }
}
