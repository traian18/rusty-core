use std::sync::Arc;

use rusty_agent_core::behavior::{
    compile as compile_profile, default_profile, default_profile_definition, profile_json_schema,
    Action, BehaviorProfile, BehaviorRestoreError, BehaviorState, CompiledProfile,
    DefinitionStatus, EnteredFrom, EvaluatorSpec, ProfileRef, ProfileRegistry,
    ProfileRegistryError, ProfileSource, RunCounters,
};
use serde_json::{json, Value};

#[test]
fn public_facades_expose_representative_types() {
    let profile: BehaviorProfile = default_profile_definition();
    let profile_ref = ProfileRef::exact("example.profile", 1);
    let compiled: CompiledProfile =
        compile_profile(profile.clone()).expect("default profile compiles");
    let state = BehaviorState::new(default_profile());
    let action = Action::Allow {};
    let evaluator = EvaluatorSpec::Tool {
        tool: "check".into(),
        args: Value::Null,
        answer_arg: None,
    };

    assert_eq!(profile_ref.to_string(), "example.profile@1");
    assert_eq!(compiled.reference().id, profile.id);
    assert_eq!(state.run, RunCounters::default());
    assert_eq!(action.name(), "allow");
    assert_eq!(evaluator.type_name(), "tool");
}

#[test]
fn behavior_profile_and_state_round_trip_through_public_apis() {
    let mut profile = default_profile_definition();
    profile.rules = vec![rusty_agent_core::behavior::Rule {
        id: "inject-guidance".into(),
        on: rusty_agent_core::behavior::RuleEvent::RunStart,
        when: None,
        action: Action::Inject(rusty_agent_core::behavior::Inject {
            text: "Be precise".into(),
            placement: None,
        }),
        max_fires: Some(1),
        metadata: Value::Null,
    }];
    profile.completion_gate = Some(rusty_agent_core::behavior::CompletionGate {
        checks: vec![rusty_agent_core::behavior::GateCheck {
            id: "quality".into(),
            require: None,
            evaluator: Some(EvaluatorSpec::Model {
                instructions: "Check the answer".into(),
                model: None,
                transcript_messages: 10,
            }),
            feedback: None,
            error_policy: Default::default(),
            waive_when_exhausted: false,
            metadata: Value::Null,
        }],
        max_continuations: 3,
        on_exhausted: None,
    });

    let encoded = serde_json::to_value(&profile).expect("serialize profile");
    assert_eq!(encoded["rules"][0]["do"]["inject"]["text"], "Be precise");
    assert_eq!(
        encoded["completion_gate"]["checks"][0]["evaluator"]["type"],
        "model"
    );
    assert_eq!(
        serde_json::from_value::<BehaviorProfile>(encoded).unwrap(),
        profile
    );

    let mut state = BehaviorState::new(default_profile());
    state.run.turns = 2;
    state.run.tool_calls = 1;
    state.run.fired.insert("rule".into(), 1);
    state.run.offered_tools = vec!["read".into()];
    state.run.final_status = Some("completed".into());
    let stored = state.to_stored();
    let restored = BehaviorState::from_stored(Some(&stored)).expect("restore behavior state");
    assert_eq!(restored.run, state.run);
    assert_eq!(restored.profile.profile, state.profile.profile);
    assert_eq!(
        BehaviorState::from_stored(None).unwrap().run,
        RunCounters::default()
    );
    assert_eq!(
        BehaviorState::from_stored(Some(&Value::Null)).unwrap().run,
        RunCounters::default()
    );
}

#[test]
fn behavior_state_restore_preserves_failure_contracts_and_stored_fields() {
    let mut state = BehaviorState::new(default_profile());
    state.commands_trusted = true;
    let mut library_profile = default_profile_definition();
    library_profile.id = "example.library".into();
    Arc::make_mut(&mut state.library)
        .register(library_profile, ProfileSource::Host)
        .expect("register library profile");
    state.entered_from = Some(EnteredFrom {
        profile: ProfileRef::exact("previous", 1),
        name: "Previous profile".into(),
    });
    state.run.turns = 3;
    state.run.final_status = Some("completed".into());

    let stored = state.to_stored();
    for key in [
        "profile",
        "content_hash",
        "run",
        "library",
        "entered_from",
        "commands_trusted",
    ] {
        assert!(stored.get(key).is_some(), "missing persisted field {key}");
    }
    let restored = BehaviorState::from_stored(Some(&stored)).expect("restore state");
    assert_eq!(restored.profile.profile, state.profile.profile);
    assert_eq!(restored.profile.content_hash, state.profile.content_hash);
    assert_eq!(restored.run, state.run);
    assert_eq!(
        restored
            .library
            .profiles()
            .map(|(profile, _)| (&profile.profile, &profile.content_hash))
            .collect::<Vec<_>>(),
        state
            .library
            .profiles()
            .map(|(profile, _)| (&profile.profile, &profile.content_hash))
            .collect::<Vec<_>>()
    );
    assert_eq!(restored.entered_from, state.entered_from);
    assert_eq!(restored.commands_trusted, state.commands_trusted);

    assert!(matches!(
        BehaviorState::from_stored(Some(&json!({ "profile": 1 }))),
        Err(BehaviorRestoreError::Malformed(_))
    ));

    let mut invalid = stored.clone();
    invalid["profile"]["name"] = json!("");
    assert!(matches!(
        BehaviorState::from_stored(Some(&invalid)),
        Err(BehaviorRestoreError::Invalid(_))
    ));

    let mut tampered = stored.clone();
    tampered["profile"]["limits"]["max_turns"] = json!(500);
    assert!(matches!(
        BehaviorState::from_stored(Some(&tampered)),
        Err(BehaviorRestoreError::HashMismatch(_))
    ));

    let mut invalid_library = stored;
    let mut library_profile = default_profile_definition();
    library_profile.id = "example.invalid".into();
    library_profile.name.clear();
    invalid_library["library"] = json!([library_profile]);
    assert!(matches!(
        BehaviorState::from_stored(Some(&invalid_library)),
        Err(BehaviorRestoreError::Library(_))
    ));
}

#[test]
fn profile_registry_retains_conflict_and_resolution_contracts() {
    let mut registry = ProfileRegistry::new();
    let mut profile = default_profile_definition();
    profile.id = "example.profile".into();
    registry
        .register(profile.clone(), ProfileSource::Host)
        .expect("register profile");

    let mut changed = profile.clone();
    changed.name = "Changed".into();
    assert!(matches!(
        registry.register(changed, ProfileSource::Host),
        Err(ProfileRegistryError::ImmutableRevision(_))
    ));

    let mut reserved = default_profile_definition();
    reserved.id = "rusty.custom".into();
    assert!(matches!(
        registry.register(reserved, ProfileSource::Host),
        Err(ProfileRegistryError::Reserved(_))
    ));

    assert!(matches!(
        registry.resolve(&ProfileRef::exact("missing.profile", 1)),
        Err(ProfileRegistryError::NotFound(_))
    ));

    let mut draft = profile;
    draft.revision = 2;
    draft.status = DefinitionStatus::Draft;
    registry
        .register(draft, ProfileSource::Host)
        .expect("register draft");
    assert!(matches!(
        registry.resolve(&ProfileRef::exact("example.profile", 2)),
        Err(ProfileRegistryError::DraftNotExecutable(_))
    ));
    assert_eq!(
        registry
            .allow_drafts(true)
            .resolve(&ProfileRef::exact("example.profile", 2))
            .unwrap()
            .profile
            .revision,
        2
    );
}

#[test]
fn behavior_schema_retains_public_shape() {
    let schema = serde_json::to_value(profile_json_schema()).expect("serialize profile schema");
    assert!(schema["properties"]["rules"].is_object());
    assert!(schema["definitions"].get("Action").is_some());
    assert!(schema["definitions"].get("EvaluatorSpec").is_some());
}
