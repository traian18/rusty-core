//! Definition identifiers and publication metadata.

use serde::{Deserialize, Serialize};

pub const ORCHESTRATION_SCHEMA_VERSION: u32 = 1;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

string_id!(OrchestrationDefinitionId);
string_id!(OrchestrationNodeId);
string_id!(OrchestrationEdgeId);
string_id!(OrchestrationRunId);

/// Publication state of a definition revision.
///
/// Published revisions are immutable; drafts only execute when the registry
/// explicitly allows them (development mode).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DefinitionStatus {
    Draft,
    #[default]
    Published,
    Deprecated,
}
