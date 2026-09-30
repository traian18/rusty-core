use harness_protocol::effects::SpawnAgentSpec;

use super::definition::ChildPolicy;
use super::state::BehaviorState;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChildPolicyError {
    #[error("child policy `{0}` is not supported")]
    Unsupported(&'static str),
    #[error("{0}")]
    Rejected(String),
}

/// Decides the behavior a spawned child starts with. Pluggable per session
/// so hosts can define inheritance before the JSON format grows it
/// (`BEHAVIOR_LAYER_DESIGN.md` §8a).
pub trait ChildBehaviorResolver: Send + Sync + std::fmt::Debug {
    fn resolve(
        &self,
        parent: &BehaviorState,
        spec: &SpawnAgentSpec,
    ) -> Result<BehaviorState, ChildPolicyError>;
}

/// Follows the parent profile's `children` policy. The default resolver.
#[derive(Debug, Clone, Copy, Default)]
pub struct PolicyChildBehaviorResolver;

impl ChildBehaviorResolver for PolicyChildBehaviorResolver {
    fn resolve(
        &self,
        parent: &BehaviorState,
        spec: &SpawnAgentSpec,
    ) -> Result<BehaviorState, ChildPolicyError> {
        match &parent.profile.profile.children {
            ChildPolicy::Inherit => InheritParentProfile.resolve(parent, spec),
            // The compiler rejects reserved policies, so a compiled profile
            // cannot reach this; keep the failure explicit regardless.
            other => Err(ChildPolicyError::Unsupported(other.type_name())),
        }
    }
}

/// The child runs the parent's active profile with fresh counters.
#[derive(Debug, Clone, Copy, Default)]
pub struct InheritParentProfile;

impl ChildBehaviorResolver for InheritParentProfile {
    fn resolve(
        &self,
        parent: &BehaviorState,
        _spec: &SpawnAgentSpec,
    ) -> Result<BehaviorState, ChildPolicyError> {
        Ok(BehaviorState::new(parent.profile.clone())
            .with_library(parent.library.clone())
            .with_commands_trusted(parent.commands_trusted))
    }
}
