use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Publication state of a definition revision.
///
/// Published revisions are immutable; drafts only execute when the registry
/// explicitly allows them (development mode).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DefinitionStatus {
    Draft,
    #[default]
    Published,
    Deprecated,
}
