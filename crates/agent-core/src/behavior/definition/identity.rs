//! Profile identifiers and revision references.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ProfileId(pub String);

impl ProfileId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ProfileId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl std::fmt::Display for ProfileId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Reference to a profile revision. Without a revision it means the highest
/// published revision of `id` (or the newest draft, where drafts are allowed).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct ProfileRef {
    pub id: ProfileId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

impl ProfileRef {
    pub fn exact(id: impl Into<String>, revision: u64) -> Self {
        Self {
            id: ProfileId::new(id),
            revision: Some(revision),
        }
    }

    pub fn latest(id: impl Into<String>) -> Self {
        Self {
            id: ProfileId::new(id),
            revision: None,
        }
    }
}

impl std::fmt::Display for ProfileRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.revision {
            Some(revision) => write!(formatter, "{}@{revision}", self.id),
            None => write!(formatter, "{}@latest", self.id),
        }
    }
}
