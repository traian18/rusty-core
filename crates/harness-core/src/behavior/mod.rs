//! Behavior profiles: what an agent is told, which tools it may use, and how
//! its loop is bounded (`BEHAVIOR_LAYER_DESIGN.md`).
//!
//! Pure, like the rest of the core. `transitions.rs` consults the agent's
//! [`BehaviorState`] when it builds requests and admits tool calls.

mod children;
mod compiler;
mod default;
mod definition;
mod registry;
mod rules;
mod state;

pub use children::{
    ChildBehaviorResolver, ChildPolicyError, InheritParentProfile, PolicyChildBehaviorResolver,
};
pub use compiler::{compile, CompiledProfile, ProfileIssue, ProfileValidationError};
pub use default::{
    default_profile, default_profile_definition, DEFAULT_PROFILE_ID, RESERVED_PROFILE_PREFIX,
};
pub use definition::*;
pub use registry::{ProfileRegistry, ProfileRegistryError, ProfileSource};
pub use rules::{
    merge_patch, system_reminder, FiredRule, Injection, ResultRewrite, RuleOutcome, RuleStop,
    ToolDecision, ToolEvent,
};
pub use state::{
    BehaviorRestoreError, BehaviorState, CallStreak, EnteredFrom, ExecutedCall, RunCounters,
    TurnPlan, MAX_SWITCHES_PER_RUN,
};

/// JSON Schema of the profile document, for editors.
pub fn profile_json_schema() -> schemars::schema::RootSchema {
    schemars::schema_for!(BehaviorProfile)
}

#[cfg(test)]
mod tests;
