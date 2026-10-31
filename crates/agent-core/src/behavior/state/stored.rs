//! The durable form of a behavior state, and restoring from it.

use super::{BehaviorState, EnteredFrom, RunCounters};
use crate::behavior::compiler::{compile, ProfileValidationError};
use crate::behavior::definition::{BehaviorProfile, ProfileRef};
use crate::behavior::registry::{ProfileRegistry, ProfileRegistryError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

impl BehaviorState {
    /// Durable form: the full profile document (so a snapshot is
    /// self-contained), its hash, and the counters.
    pub fn to_stored(&self) -> serde_json::Value {
        serde_json::to_value(StoredBehavior {
            profile: self.profile.profile.clone(),
            content_hash: self.profile.content_hash.clone(),
            run: self.run.clone(),
            library: self.library.documents(),
            entered_from: self.entered_from.clone(),
            commands_trusted: self.commands_trusted,
        })
        .expect("behavior state always serializes")
    }

    /// Rebuild from [`to_stored`](Self::to_stored). The profile is
    /// recompiled and its hash must match, so a tampered or corrupted
    /// snapshot fails closed instead of running under different rules.
    /// `None` (a snapshot from before the behavior layer) restores the
    /// built-in default.
    pub fn from_stored(value: Option<&serde_json::Value>) -> Result<Self, BehaviorRestoreError> {
        let Some(value) = value.filter(|value| !value.is_null()) else {
            return Ok(Self::default());
        };
        let stored: StoredBehavior = serde_json::from_value(value.clone())
            .map_err(|error| BehaviorRestoreError::Malformed(error.to_string()))?;
        let compiled = compile(stored.profile).map_err(BehaviorRestoreError::Invalid)?;
        if compiled.content_hash != stored.content_hash {
            return Err(BehaviorRestoreError::HashMismatch(compiled.reference()));
        }
        let library =
            ProfileRegistry::from_library(stored.library).map_err(BehaviorRestoreError::Library)?;
        Ok(Self {
            profile: Arc::new(compiled),
            library: Arc::new(library),
            entered_from: stored.entered_from,
            commands_trusted: stored.commands_trusted,
            run: stored.run,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredBehavior {
    profile: BehaviorProfile,
    content_hash: String,
    run: RunCounters,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    library: Vec<BehaviorProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    entered_from: Option<EnteredFrom>,
    #[serde(default)]
    commands_trusted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BehaviorRestoreError {
    #[error("stored behavior state is malformed: {0}")]
    Malformed(String),
    #[error("stored behavior profile is no longer valid: {0}")]
    Invalid(ProfileValidationError),
    #[error("stored behavior profile {0} does not match its recorded content hash")]
    HashMismatch(ProfileRef),
    #[error("stored behavior profile library is no longer valid: {0}")]
    Library(ProfileRegistryError),
}
