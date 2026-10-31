//! Schema resolution and host-side JSON Schema validation.
//!
//! Provider-side structured output is helpful but never authoritative: every structured boundary
//! is validated here. The validator implements exactly
//! [`SUPPORTED_SCHEMA_KEYWORDS`](rusty_workflow_core::SUPPORTED_SCHEMA_KEYWORDS); the compiler
//! rejects any other keyword, so no schema is ever partially interpreted.

mod resolver;
#[cfg(test)]
mod tests;
mod validate;

pub use resolver::{InMemorySchemaResolver, SchemaResolutionError, SchemaResolver};
pub use validate::{BasicSchemaValidator, SchemaValidationError, SchemaValidator};
