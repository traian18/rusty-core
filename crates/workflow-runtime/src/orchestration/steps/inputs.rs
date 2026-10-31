//! Resolving a step's bound inputs from the run, and the deterministic input node.

use rusty_workflow_core::{
    InputBinding, InputNodeConfig, OrchestrationError, OrchestrationRunState, OutputBinding,
    RetryReason,
};
use serde_json::{Map, Value};

use crate::orchestration::schema::{SchemaResolver, SchemaValidator};

pub(crate) fn resolve_inputs(
    state: &OrchestrationRunState,
    bindings: &[InputBinding],
) -> Result<Value, OrchestrationError> {
    let mut object = Map::new();
    for binding in bindings {
        object.insert(
            binding.target.clone(),
            resolve_binding(state, &binding.source)?,
        );
    }
    Ok(Value::Object(object))
}

pub(crate) fn resolve_binding(
    state: &OrchestrationRunState,
    binding: &OutputBinding,
) -> Result<Value, OrchestrationError> {
    let (value, pointer) = match binding {
        OutputBinding::RunInput { pointer } => (
            state.input.as_ref().ok_or_else(|| {
                OrchestrationError::new("missing_input", "run input is unavailable")
            })?,
            pointer,
        ),
        OutputBinding::NodeOutput { node_id, pointer } => (
            state
                .steps
                .get(node_id)
                .and_then(|step| step.output.as_ref())
                .ok_or_else(|| {
                    OrchestrationError::new(
                        "missing_output",
                        format!("node {node_id} has no output"),
                    )
                })?,
            pointer,
        ),
    };
    pointer_value(value, pointer).cloned()
}

fn pointer_value<'a>(value: &'a Value, pointer: &str) -> Result<&'a Value, OrchestrationError> {
    value.pointer(pointer).ok_or_else(|| {
        OrchestrationError::new(
            "invalid_pointer",
            format!("JSON pointer {pointer} did not resolve"),
        )
    })
}

pub(crate) struct Validation<'a> {
    pub schemas: &'a dyn SchemaResolver,
    pub validator: &'a dyn SchemaValidator,
}

impl Validation<'_> {
    pub fn resolve(
        &self,
        reference: &rusty_workflow_core::SchemaReference,
    ) -> Result<Value, OrchestrationError> {
        self.schemas
            .resolve(reference)
            .map_err(|error| OrchestrationError::new("schema_resolution_failed", error.message))
    }

    pub fn check(
        &self,
        schema: &Value,
        value: &Value,
        code: &str,
        retry_reason: Option<RetryReason>,
    ) -> Result<(), OrchestrationError> {
        self.validator.validate(schema, value).map_err(|error| {
            let message = error.to_string();
            match retry_reason {
                Some(reason) => OrchestrationError::retryable(code, message, reason),
                None => OrchestrationError::new(code, message),
            }
        })
    }
}

pub(crate) fn execute_input(
    state: &OrchestrationRunState,
    config: &InputNodeConfig,
    input_schema: Option<&rusty_workflow_core::SchemaReference>,
    validation: &Validation<'_>,
) -> Result<Value, OrchestrationError> {
    let mut input = state.input.clone().unwrap_or(Value::Null);
    if !config.defaults.is_empty() {
        let object = input.as_object_mut().ok_or_else(|| {
            OrchestrationError::new("invalid_input", "input defaults require an object input")
        })?;
        for (key, value) in &config.defaults {
            object.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    if let Some(reference) = input_schema {
        let schema = validation.resolve(reference)?;
        validation.check(&schema, &input, "invalid_input", None)?;
    }
    Ok(input)
}
