//! Which behavior profile a step runs under.

use std::sync::Arc;

use rusty_agent_core::behavior::{BehaviorProfile, CompiledProfile, ProfileRegistryError};
use serde_json::Value;

use super::IsolatedSessionAgentExecutor;
use crate::orchestration::{AgentExecutionError, AgentStepRequest};

/// A compiled profile with the documents of the switch library it can reach.
pub(super) type StepProfile = (Arc<CompiledProfile>, Vec<Value>);

impl IsolatedSessionAgentExecutor {
    /// The step's profile and switch library: the profile it names (with its switch closure),
    /// else the parent's active profile and library.
    pub(super) fn step_profile(
        &self,
        request: &AgentStepRequest,
    ) -> Result<StepProfile, AgentExecutionError> {
        let Some(reference) = &request.profile else {
            let behavior = self.parent.root_behavior();
            let library = behavior.library.documents().iter().map(document).collect();
            return Ok((behavior.profile, library));
        };
        let registry = self.profiles.as_ref().ok_or_else(|| {
            AgentExecutionError::new(
                "unknown_profile",
                format!("step names profile {reference} but no profile registry is configured"),
            )
        })?;
        let unknown = |error: ProfileRegistryError| {
            AgentExecutionError::new("unknown_profile", error.to_string())
        };
        let profile = registry.resolve(reference).map_err(unknown)?;
        let closure = registry.resolve_closure(&profile).map_err(unknown)?;
        let library = closure[1..]
            .iter()
            .map(|profile| document(&profile.profile))
            .collect();
        Ok((profile, library))
    }
}

/// The profile as the JSON document a session takes.
pub(super) fn document(profile: &BehaviorProfile) -> Value {
    serde_json::to_value(profile).unwrap_or(Value::Null)
}
