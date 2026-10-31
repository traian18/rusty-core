//! Resolving a schema reference to the schema itself.

use std::collections::BTreeMap;

use rusty_workflow_core::{
    task_plan_schema, SchemaReference, TASK_PLAN_SCHEMA_ID, TASK_PLAN_SCHEMA_REVISION,
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
