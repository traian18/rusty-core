use std::sync::{Arc, OnceLock};

use serde_json::Value;

use crate::orchestration::{DefinitionStatus, ToolScope};

use super::compiler::{compile, CompiledProfile};
use super::definition::*;

/// Id of the built-in, behavior-neutral profile every agent runs under when
/// nothing else is chosen.
pub const DEFAULT_PROFILE_ID: &str = "rusty.default";

/// Ids with this prefix are reserved for built-in profiles.
pub const RESERVED_PROFILE_PREFIX: &str = "rusty.";

/// The built-in default profile. It is deliberately neutral — no
/// instructions, inherited tools and parameters, no limits — so running an
/// agent under it is indistinguishable from running it without the behavior
/// layer.
pub fn default_profile_definition() -> BehaviorProfile {
    BehaviorProfile {
        schema_version: BEHAVIOR_SCHEMA_VERSION,
        id: ProfileId::from(DEFAULT_PROFILE_ID),
        revision: 1,
        name: "Default".into(),
        description: Some("Behavior-neutral built-in profile".into()),
        status: DefinitionStatus::Published,
        instructions: Instructions::default(),
        tools: ToolScope::Inherit,
        tool_overrides: Default::default(),
        execution: ExecutionOverlay::default(),
        limits: Limits::default(),
        rules: Vec::new(),
        completion_gate: None,
        children: ChildPolicy::Inherit,
        metadata: Value::Null,
    }
}

/// The compiled built-in default, shared by every agent that uses it.
pub fn default_profile() -> Arc<CompiledProfile> {
    static DEFAULT: OnceLock<Arc<CompiledProfile>> = OnceLock::new();
    DEFAULT
        .get_or_init(|| {
            Arc::new(compile(default_profile_definition()).expect("built-in profile is valid"))
        })
        .clone()
}
