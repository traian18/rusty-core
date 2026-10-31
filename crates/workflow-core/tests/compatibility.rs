use rusty_agent_core::behavior::ProfileRef;
use rusty_workflow_core::{
    compile, default_orchestration_definition, is_task_plan_schema, task_plan_schema,
    DefinitionRef, DefinitionRegistry, DefinitionStatus, OrchestrationDefinition,
    OrchestrationDefinitionId, OrchestrationNodeId, OrchestrationRunId, OrchestrationRunState,
    OutputBinding, RegistryError, SchemaReference, SubflowTarget, TaskPlannerConfig,
    TaskQueueConfig, ToolScope, ORCHESTRATION_SCHEMA_VERSION, TASK_PLAN_SCHEMA_ID,
    TASK_PLAN_SCHEMA_REVISION,
};
use serde_json::json;

#[test]
fn public_facade_exposes_representative_types() {
    let definition = default_orchestration_definition();
    let run = OrchestrationRunState::new(
        OrchestrationRunId::from("run-1"),
        &compile(definition).expect("default orchestration compiles"),
    );
    let definition_registry = DefinitionRegistry::new();
    let task_schema = task_plan_schema();

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

    let approval = rusty_workflow_core::ApprovalNodeConfig {
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
