use std::sync::Arc;

use harness_core::{
    behavior::{
        compile as compile_profile, default_profile, default_profile_definition,
        profile_json_schema, Action, BehaviorProfile, BehaviorRestoreError, BehaviorState,
        CompiledProfile, EnteredFrom, EvaluatorSpec, ProfileRef, ProfileRegistry,
        ProfileRegistryError, ProfileSource, RunCounters,
    },
    orchestration::{
        compile, default_orchestration_definition, is_task_plan_schema, task_plan_schema,
        DefinitionRef, DefinitionRegistry, DefinitionStatus, OrchestrationDefinition,
        OrchestrationDefinitionId, OrchestrationNodeId, OrchestrationRunId, OrchestrationRunState,
        OutputBinding, RegistryError, SchemaReference, SubflowTarget, TaskPlannerConfig,
        TaskQueueConfig, ToolScope, ORCHESTRATION_SCHEMA_VERSION, TASK_PLAN_SCHEMA_ID,
        TASK_PLAN_SCHEMA_REVISION,
    },
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
    };

    let definition = default_orchestration_definition();
    let run = OrchestrationRunState::new(
        OrchestrationRunId::from("run-1"),
        &compile(definition).expect("default orchestration compiles"),
    );
    let definition_registry = DefinitionRegistry::new();
    let task_schema = task_plan_schema();

    assert_eq!(profile_ref.to_string(), "example.profile@1");
    assert_eq!(compiled.reference().id, profile.id);
    assert_eq!(state.run, RunCounters::default());
    assert_eq!(action.name(), "allow");
    assert_eq!(evaluator.type_name(), "tool");
    assert_eq!(run.run_id.as_str(), "run-1");
    assert!(definition_registry
        .allow_drafts(false)
        .resolve(&DefinitionRef::LatestPublished(
            OrchestrationDefinitionId::from("missing")
        ))
        .is_err());
    assert_eq!(task_schema["type"], "object");
}

#[test]
fn behavior_profile_and_state_round_trip_through_public_apis() {
    let mut profile = default_profile_definition();
    profile.rules = vec![harness_core::behavior::Rule {
        id: "inject-guidance".into(),
        on: harness_core::behavior::RuleEvent::RunStart,
        when: None,
        action: Action::Inject(harness_core::behavior::Inject {
            text: "Be precise".into(),
            placement: None,
        }),
        max_fires: Some(1),
        metadata: Value::Null,
    }];
    profile.completion_gate = Some(harness_core::behavior::CompletionGate {
        checks: vec![harness_core::behavior::GateCheck {
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

#[test]
fn orchestration_definition_and_state_round_trip() {
    let definition = default_orchestration_definition();
    let encoded = serde_json::to_value(&definition).unwrap();
    assert_eq!(encoded["schema_version"], ORCHESTRATION_SCHEMA_VERSION);
    assert_eq!(encoded["id"], "rusty.default");
    let decoded: OrchestrationDefinition = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, definition);

    let compiled = compile(definition.clone()).expect("default orchestration compiles");
    let recompiled = compile(definition).expect("same orchestration compiles again");
    assert_eq!(compiled.content_hash, recompiled.content_hash);

    let mut state = OrchestrationRunState::new(OrchestrationRunId::from("run-1"), &compiled);
    state.input = Some(json!({"request": "hello"}));
    state.paused = true;
    state.total_attempts = 2;
    state.options.auto_approve = true;
    let state_json = serde_json::to_value(&state).unwrap();
    for key in [
        "run_id",
        "definition_id",
        "definition_revision",
        "definition_hash",
        "status",
        "steps",
        "options",
    ] {
        assert!(
            state_json.get(key).is_some(),
            "missing persisted field {key}"
        );
    }
    assert_eq!(
        serde_json::from_value::<OrchestrationRunState>(state_json).unwrap(),
        state
    );
}

#[test]
fn step_subflow_generation_retains_structure() {
    let target = SubflowTarget::Step {
        instructions: "Review the input".into(),
        profile: Some(ProfileRef::latest("reviewer")),
        tools: ToolScope::AllowList(vec!["read".into()]),
        model: Some("model-x".into()),
    };
    let definition = target
        .step_definition(
            &OrchestrationNodeId::from("review"),
            &["input".into(), "context".into()],
        )
        .expect("step target generates a definition");
    assert_eq!(definition.id.as_str(), "subflow.step.review");
    assert_eq!(definition.revision, 1);
    assert_eq!(definition.name, "Step review");
    assert_eq!(definition.nodes.len(), 3);
    assert_eq!(
        definition
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>(),
        ["input", "step", "output"]
    );
    assert_eq!(
        definition
            .edges
            .iter()
            .map(|edge| edge.id.as_str())
            .collect::<Vec<_>>(),
        ["input-step", "step-output"]
    );
    assert_eq!(definition.nodes[1].input_bindings.len(), 2);
    assert_eq!(definition.nodes[1].kind.type_name(), "agent");
    assert!(SubflowTarget::Flow {
        id: OrchestrationDefinitionId::from("saved"),
        revision: None
    }
    .step_definition(&OrchestrationNodeId::from("review"), &[])
    .is_none());
}

#[test]
fn approval_and_task_queue_helpers_retain_behavior() {
    let review = ProfileRef::latest("review");
    let planner = ProfileRef::latest("planner");
    let queue = TaskQueueConfig {
        plan_pointer: "/requirements/tasks".into(),
        review_profile: review.clone(),
        review_instructions: "Review".into(),
        task_planner: Some(TaskPlannerConfig {
            profile: planner.clone(),
            instructions: "Plan".into(),
        }),
        max_repairs: 2,
        on_task_failure: Default::default(),
        flows: Default::default(),
    };
    assert_eq!(
        queue.profiles().cloned().collect::<Vec<_>>(),
        vec![review, planner]
    );
    assert_eq!(queue.plan_binding(), Some("requirements"));
    assert_eq!(queue.plan_pointer, "/requirements/tasks");
    assert_eq!(
        TaskQueueConfig {
            plan_pointer: "requirements".into(),
            ..queue.clone()
        }
        .plan_binding(),
        None
    );

    let approval = harness_core::orchestration::ApprovalNodeConfig {
        subject: OutputBinding::NodeOutput {
            node_id: OrchestrationNodeId::from("step"),
            pointer: "".into(),
        },
        prompt: String::new(),
        revise_target: None,
        max_revisions: 5,
        allow_auto_approve: true,
        skip_if_empty: None,
        revise_on_notes: false,
    };
    assert_eq!(approval.revise_target().unwrap().as_str(), "step");
}

#[test]
fn orchestration_compiler_retains_error_contract() {
    let mut definition = default_orchestration_definition();
    definition.schema_version = 99;
    definition.name.clear();
    let error = compile(definition).expect_err("invalid definition rejected");
    assert_eq!(
        error
            .issues
            .iter()
            .map(|issue| (issue.path.as_str(), issue.code))
            .collect::<Vec<_>>(),
        vec![
            ("schema_version", "unsupported_schema_version"),
            ("name", "empty_name")
        ]
    );
    assert!(error
        .issues
        .iter()
        .any(|issue| issue.path == "schema_version" && issue.code == "unsupported_schema_version"));
    assert!(error
        .issues
        .iter()
        .any(|issue| issue.path == "name" && issue.code == "empty_name"));
    assert!(error
        .to_string()
        .contains("invalid orchestration definition"));
}

#[test]
fn orchestration_registry_retains_resolution_and_error_contracts() {
    let mut registry = DefinitionRegistry::new();
    let mut published = default_orchestration_definition();
    published.id = OrchestrationDefinitionId::from("example");
    registry.register(published.clone()).unwrap();
    let mut draft = published.clone();
    draft.revision = 2;
    draft.status = DefinitionStatus::Draft;
    registry.register(draft.clone()).unwrap();

    let id = OrchestrationDefinitionId::from("example");
    assert_eq!(
        registry
            .resolve(&DefinitionRef::LatestPublished(id.clone()))
            .unwrap()
            .revision,
        1
    );
    assert!(matches!(
        registry.resolve(&DefinitionRef::Exact {
            id: id.clone(),
            revision: 2
        }),
        Err(RegistryError::DraftNotExecutable { .. })
    ));
    assert!(registry
        .resolve(&DefinitionRef::Exact {
            id: OrchestrationDefinitionId::from("missing"),
            revision: 1
        })
        .is_err());
    assert_eq!(
        registry
            .allow_drafts(true)
            .resolve(&DefinitionRef::Latest(id))
            .unwrap()
            .revision,
        2
    );
}

#[test]
fn task_plan_schema_helpers_retain_public_contract() {
    let schema = task_plan_schema();
    assert_eq!(TASK_PLAN_SCHEMA_ID, "rusty.task_plan");
    assert_eq!(TASK_PLAN_SCHEMA_REVISION, 1);
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);
    for required in ["status", "summary", "requirements", "tasks"] {
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == required));
    }
    let registry = SchemaReference::Registry {
        schema_id: TASK_PLAN_SCHEMA_ID.into(),
        revision: TASK_PLAN_SCHEMA_REVISION,
    };
    let inline = SchemaReference::Inline {
        name: "plan".into(),
        schema: schema.clone(),
    };
    assert!(is_task_plan_schema(&registry));
    assert!(is_task_plan_schema(&inline));
    assert!(!is_task_plan_schema(&SchemaReference::Registry {
        schema_id: "other".into(),
        revision: 1
    }));
}
