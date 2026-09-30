use serde_json::{json, Value};

use super::*;

fn compiled_default() -> CompiledOrchestration {
    compile(default_orchestration_definition()).expect("default definition compiles")
}

fn id(value: &str) -> OrchestrationNodeId {
    OrchestrationNodeId::from(value)
}

fn report(status: &str) -> Value {
    json!({ "summary": "Done", "status": status, "artifacts": [], "claimsToVerify": [] })
}

fn started_with(compiled: &CompiledOrchestration) -> OrchestrationRunState {
    let mut state = OrchestrationRunState::new(OrchestrationRunId::from("run-1"), compiled);
    apply(
        compiled,
        &mut state,
        OrchestrationCommand::Start {
            input: json!({ "request": "Build it" }),
        },
    )
    .expect("run starts");
    state
}

fn started() -> (CompiledOrchestration, OrchestrationRunState) {
    let compiled = compiled_default();
    let state = started_with(&compiled);
    (compiled, state)
}

fn admit(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node: &str,
    attempt: u32,
) -> Vec<OrchestrationEffect> {
    apply(
        compiled,
        state,
        OrchestrationCommand::StepAdmitted {
            node_id: id(node),
            attempt,
        },
    )
    .expect("step admitted")
}

fn succeed_with(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node: &str,
    attempt: u32,
    output: Value,
    details: AttemptDetails,
) -> Vec<OrchestrationEffect> {
    apply(
        compiled,
        state,
        OrchestrationCommand::StepSucceeded {
            node_id: id(node),
            attempt,
            output,
            details,
        },
    )
    .expect("step succeeds")
}

fn succeed(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node: &str,
    attempt: u32,
    output: Value,
) -> Vec<OrchestrationEffect> {
    succeed_with(
        compiled,
        state,
        node,
        attempt,
        output,
        AttemptDetails::default(),
    )
}

fn fail(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node: &str,
    attempt: u32,
    error: OrchestrationError,
) -> Vec<OrchestrationEffect> {
    apply(
        compiled,
        state,
        OrchestrationCommand::StepFailed {
            node_id: id(node),
            attempt,
            error,
            details: AttemptDetails::default(),
        },
    )
    .expect("step fails")
}

fn run_step(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    node: &str,
    output: Value,
) {
    let attempt = state.steps[&id(node)].attempts.len() as u32 + 1;
    admit(compiled, state, node, attempt);
    succeed(compiled, state, node, attempt, output);
}

fn verification_failed() -> OrchestrationError {
    OrchestrationError::retryable(
        "verification_failed",
        "status must be completed",
        RetryReason::VerificationFailed,
    )
}

fn codes(error: &DefinitionValidationError) -> Vec<&'static str> {
    error.issues.iter().map(|issue| issue.code).collect()
}

fn events(effects: &[OrchestrationEffect]) -> Vec<&OrchestrationEvent> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            OrchestrationEffect::Emit(event) => Some(event),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Definition and compiler
// ---------------------------------------------------------------------------

#[test]
fn default_definition_round_trips_and_compiles() {
    let definition = default_orchestration_definition();
    let encoded = serde_json::to_string_pretty(&definition).expect("serialize definition");
    let decoded: OrchestrationDefinition =
        serde_json::from_str(&encoded).expect("deserialize definition");
    let compiled = compile(decoded).expect("compile definition");

    assert_eq!(compiled.entry_node, id("input"));
    assert_eq!(compiled.nodes.len(), 4);
    assert_eq!(compiled.topological_rank.len(), 4);
    assert_eq!(
        compiled.retry_spans[&id("verify")],
        vec![id("execute"), id("verify")]
    );
    assert_eq!(compiled.content_hash, compiled_default().content_hash);
    assert!(compiled.content_hash.starts_with("fnv1a64:"));
}

#[test]
fn content_hash_changes_with_content() {
    let mut definition = default_orchestration_definition();
    definition.name = "Renamed".into();
    assert_ne!(
        compile(definition).unwrap().content_hash,
        compiled_default().content_hash
    );
}

#[test]
fn unknown_node_kinds_fail_closed() {
    let mut encoded = serde_json::to_value(default_orchestration_definition()).unwrap();
    encoded["nodes"][1]["type"] = json!("parallel");
    assert!(serde_json::from_value::<OrchestrationDefinition>(encoded).is_err());
}

#[test]
fn editor_metadata_is_preserved_and_ignored() {
    let mut definition = default_orchestration_definition();
    definition.nodes[1].metadata = json!({ "editor": { "position": { "x": 420, "y": 180 } } });
    let decoded: OrchestrationDefinition =
        serde_json::from_str(&serde_json::to_string(&definition).unwrap()).unwrap();
    assert_eq!(decoded.nodes[1].metadata, definition.nodes[1].metadata);
    compile(decoded).expect("metadata does not affect semantics");
}

#[test]
fn compiler_reports_multiple_graph_issues() {
    let mut definition = default_orchestration_definition();
    definition.schema_version = 99;
    definition.nodes.retain(|node| node.id != id("output"));
    definition.edges.push(OrchestrationEdge {
        id: OrchestrationEdgeId::from("dangling"),
        source: id("missing"),
        target: id("also-missing"),
        condition: EdgeCondition::OnSuccess,
        metadata: Value::Null,
    });

    let error = compile(definition).expect_err("invalid graph rejected");
    let codes = codes(&error);
    assert!(codes.contains(&"unsupported_schema_version"));
    assert!(codes.contains(&"missing_output"));
    assert!(codes.contains(&"dangling_edge"));
}

#[test]
fn compiler_rejects_cycles_and_ambiguous_transitions() {
    let mut definition = default_orchestration_definition();
    definition.edges.push(OrchestrationEdge {
        id: OrchestrationEdgeId::from("verify-execute"),
        source: id("verify"),
        target: id("execute"),
        condition: EdgeCondition::OnSuccess,
        metadata: Value::Null,
    });

    let error = compile(definition).expect_err("cycle rejected");
    let codes = codes(&error);
    assert!(codes.contains(&"ambiguous_transition"));
    assert!(codes.contains(&"cycle"));
}

#[test]
fn compiler_rejects_bindings_to_data_that_is_not_yet_available() {
    let mut definition = default_orchestration_definition();
    let execute = definition
        .nodes
        .iter_mut()
        .find(|node| node.id == id("execute"))
        .unwrap();
    execute.input_bindings.push(InputBinding {
        target: "verdict".into(),
        source: OutputBinding::NodeOutput {
            node_id: id("verify"),
            pointer: "/passed".into(),
        },
    });
    execute.input_bindings.push(InputBinding {
        target: "bad".into(),
        source: OutputBinding::RunInput {
            pointer: "request".into(),
        },
    });

    let codes = codes(&compile(definition).expect_err("downstream binding rejected"));
    assert!(codes.contains(&"unavailable_binding_source"));
    assert!(codes.contains(&"invalid_pointer"));
}

#[test]
fn compiler_rejects_unsupported_schema_keywords_and_unbounded_retries() {
    let mut definition = default_orchestration_definition();
    definition.output_contract.schema = SchemaReference::Inline {
        name: "loose".into(),
        schema: json!({ "type": "string", "pattern": "^a" }),
    };
    definition.nodes[1].retry.max_attempts = MAX_STEP_ATTEMPTS + 1;

    let codes = codes(&compile(definition).expect_err("rejected"));
    assert!(codes.contains(&"unsupported_schema_keyword"));
    assert!(codes.contains(&"invalid_retry"));
}

#[test]
fn compiler_validates_verification_retry_targets() {
    let set_target = |target: &str| {
        let mut definition = default_orchestration_definition();
        for node in &mut definition.nodes {
            if let OrchestrationNodeKind::Verify(config) = &mut node.kind {
                config.retry_target = Some(id(target));
            }
        }
        definition
    };
    assert!(codes(&compile(set_target("output")).unwrap_err()).contains(&"invalid_retry_target"));
    assert!(codes(&compile(set_target("nope")).unwrap_err()).contains(&"unknown_retry_target"));

    let mut definition = set_target("execute");
    for node in &mut definition.nodes {
        if node.id == id("execute") {
            node.retry
                .retry_on
                .retain(|reason| *reason != RetryReason::VerificationFailed);
        }
    }
    assert!(codes(&compile(definition).unwrap_err()).contains(&"retry_target_policy"));
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

#[test]
fn registry_enforces_immutable_revisions_and_draft_policy() {
    let mut registry = DefinitionRegistry::with_builtin();
    let default_id = OrchestrationDefinitionId::from("rusty.default");

    // Identical re-registration is idempotent; changed content is not.
    registry
        .register(default_orchestration_definition())
        .unwrap();
    let mut changed = default_orchestration_definition();
    changed.name = "Changed".into();
    assert!(matches!(
        registry.register(changed.clone()),
        Err(RegistryError::ImmutableRevision { .. })
    ));

    // A draft revision 2 is registered but neither latest nor executable.
    changed.revision = 2;
    changed.status = DefinitionStatus::Draft;
    registry.register(changed.clone()).unwrap();
    let latest = registry
        .resolve(&DefinitionRef::LatestPublished(default_id.clone()))
        .unwrap();
    assert_eq!(latest.revision, 1);
    assert!(matches!(
        registry.resolve(&DefinitionRef::Exact {
            id: default_id.clone(),
            revision: 2
        }),
        Err(RegistryError::DraftNotExecutable { .. })
    ));
    let registry = registry.allow_drafts(true);
    registry
        .resolve(&DefinitionRef::Exact {
            id: default_id,
            revision: 2,
        })
        .expect("drafts allowed in development mode");
}

#[test]
fn registry_rejects_invalid_definitions() {
    let mut definition = default_orchestration_definition();
    definition.nodes.clear();
    assert!(matches!(
        DefinitionRegistry::new().register(definition),
        Err(RegistryError::Invalid(_))
    ));
}

// ---------------------------------------------------------------------------
// Reducer: happy path
// ---------------------------------------------------------------------------

#[test]
fn default_flow_reaches_completed_output() {
    let (compiled, mut state) = started();
    assert_eq!(state.status, OrchestrationStatus::Ready);
    assert_eq!(ready_steps(&compiled, &state), vec![id("input")]);

    run_step(
        &compiled,
        &mut state,
        "input",
        json!({ "request": "Build it" }),
    );
    run_step(&compiled, &mut state, "execute", report("completed"));
    run_step(&compiled, &mut state, "verify", json!({ "passed": true }));
    admit(&compiled, &mut state, "output", 1);
    let effects = succeed(&compiled, &mut state, "output", 1, report("completed"));

    assert_eq!(state.status, OrchestrationStatus::Completed);
    assert_eq!(state.final_output, Some(report("completed")));
    let finish = effects
        .iter()
        .find_map(|effect| match effect {
            OrchestrationEffect::Finish(result) => Some(result),
            _ => None,
        })
        .expect("finish effect");
    assert_eq!(finish.status, OrchestrationOutcome::Completed);
    assert_eq!(finish.output, Some(report("completed")));
    assert_eq!(finish.run_id, OrchestrationRunId::from("run-1"));
}

#[test]
fn successful_steps_select_their_success_transition() {
    let (compiled, mut state) = started();
    admit(&compiled, &mut state, "input", 1);
    assert_eq!(state.status, OrchestrationStatus::Running);
    let effects = succeed(&compiled, &mut state, "input", 1, json!({}));
    assert!(
        events(&effects).contains(&&OrchestrationEvent::TransitionSelected {
            from: id("input"),
            to: id("execute"),
            condition: EdgeCondition::OnSuccess,
        })
    );
    assert_eq!(state.steps[&id("execute")].status, StepStatus::Ready);
    assert_eq!(state.status, OrchestrationStatus::Ready);
}

#[test]
fn failure_routes_activate_the_on_failure_target() {
    let mut definition = default_orchestration_definition();
    definition.nodes.push(OrchestrationNode {
        id: id("fallback"),
        name: "Fallback".into(),
        kind: OrchestrationNodeKind::Output(OutputNodeConfig {
            source: OutputBinding::NodeOutput {
                node_id: id("input"),
                pointer: String::new(),
            },
            strict: false,
        }),
        input_bindings: Vec::new(),
        output_schema: None,
        retry: RetryPolicy::default(),
        timeout_ms: None,
        metadata: Value::Null,
    });
    definition.edges.push(OrchestrationEdge {
        id: OrchestrationEdgeId::from("execute-fallback"),
        source: id("execute"),
        target: id("fallback"),
        condition: EdgeCondition::OnFailure,
        metadata: Value::Null,
    });
    let compiled = compile(definition).expect("failure route compiles");
    let mut state = started_with(&compiled);
    run_step(
        &compiled,
        &mut state,
        "input",
        json!({ "request": "Build it" }),
    );
    admit(&compiled, &mut state, "execute", 1);
    let effects = fail(
        &compiled,
        &mut state,
        "execute",
        1,
        OrchestrationError::new("denied", "not retryable"),
    );
    assert!(
        events(&effects).contains(&&OrchestrationEvent::TransitionSelected {
            from: id("execute"),
            to: id("fallback"),
            condition: EdgeCondition::OnFailure,
        })
    );
    assert_eq!(state.steps[&id("fallback")].status, StepStatus::Ready);
    assert_eq!(state.status, OrchestrationStatus::Ready);
}

// ---------------------------------------------------------------------------
// Reducer: retries
// ---------------------------------------------------------------------------

#[test]
fn retry_preserves_failed_attempt_and_uses_next_attempt_number() {
    let (compiled, mut state) = started();
    run_step(
        &compiled,
        &mut state,
        "input",
        json!({ "request": "Build it" }),
    );
    admit(&compiled, &mut state, "execute", 1);
    let error = OrchestrationError::retryable(
        "invalid_output",
        "missing status",
        RetryReason::InvalidStructuredOutput,
    );
    fail(&compiled, &mut state, "execute", 1, error.clone());
    let execute = id("execute");
    assert_eq!(state.steps[&execute].status, StepStatus::RetryScheduled);
    assert_eq!(state.steps[&execute].feedback, vec![error]);
    assert!(ready_steps(&compiled, &state).is_empty());

    apply(
        &compiled,
        &mut state,
        OrchestrationCommand::RetryStep {
            node_id: execute.clone(),
        },
    )
    .unwrap();
    admit(&compiled, &mut state, "execute", 2);

    let step = &state.steps[&execute];
    assert_eq!(step.attempts.len(), 2);
    assert_eq!(step.attempts[0].status, AttemptStatus::Failed);
    assert_eq!(step.attempts[1].status, AttemptStatus::Running);
}

#[test]
fn non_retryable_reasons_fail_immediately() {
    let (compiled, mut state) = started();
    run_step(&compiled, &mut state, "input", json!({}));
    admit(&compiled, &mut state, "execute", 1);
    fail(
        &compiled,
        &mut state,
        "execute",
        1,
        OrchestrationError::new("permission_denied", "user denied"),
    );
    assert_eq!(state.status, OrchestrationStatus::Failed);
    assert_eq!(state.steps[&id("verify")].status, StepStatus::Skipped);
}

#[test]
fn retry_exhaustion_fails_run_deterministically() {
    let (compiled, mut state) = started();
    run_step(
        &compiled,
        &mut state,
        "input",
        json!({ "request": "Build it" }),
    );
    let execute = id("execute");
    for attempt in 1..=2 {
        admit(&compiled, &mut state, "execute", attempt);
        fail(
            &compiled,
            &mut state,
            "execute",
            attempt,
            OrchestrationError::retryable(
                "timeout",
                "backend timeout",
                RetryReason::BackendTimeout,
            ),
        );
        if attempt == 1 {
            apply(
                &compiled,
                &mut state,
                OrchestrationCommand::RetryStep {
                    node_id: execute.clone(),
                },
            )
            .unwrap();
        }
    }

    assert_eq!(state.status, OrchestrationStatus::Failed);
    assert_eq!(state.steps[&execute].attempts.len(), 2);
}

#[test]
fn verification_failure_sends_work_back_to_the_retry_target_once() {
    let (compiled, mut state) = started();
    run_step(
        &compiled,
        &mut state,
        "input",
        json!({ "request": "Build it" }),
    );
    run_step(&compiled, &mut state, "execute", report("blocked"));
    admit(&compiled, &mut state, "verify", 1);
    let effects = fail(&compiled, &mut state, "verify", 1, verification_failed());

    assert!(
        events(&effects).contains(&&OrchestrationEvent::StepRetryScheduled {
            node_id: id("execute"),
            next_attempt: 2,
            triggered_by: id("verify"),
        })
    );
    let execute = &state.steps[&id("execute")];
    assert_eq!(execute.status, StepStatus::RetryScheduled);
    assert_eq!(execute.output, None, "stale output is cleared");
    assert_eq!(execute.feedback, vec![verification_failed()]);
    assert_eq!(state.steps[&id("verify")].status, StepStatus::Pending);
    assert_eq!(state.steps[&id("verify")].attempts.len(), 1, "history kept");

    apply(
        &compiled,
        &mut state,
        OrchestrationCommand::RetryStep {
            node_id: id("execute"),
        },
    )
    .unwrap();
    run_step(&compiled, &mut state, "execute", report("blocked"));
    admit(&compiled, &mut state, "verify", 2);
    fail(&compiled, &mut state, "verify", 2, verification_failed());

    // Execute's two attempts are spent: the run fails.
    assert_eq!(state.status, OrchestrationStatus::Failed);
    assert_eq!(state.error.as_ref().unwrap().code, "verification_failed");
}

// ---------------------------------------------------------------------------
// Reducer: permissions
// ---------------------------------------------------------------------------

fn permission(node: &str, attempt: u32, permission_id: &str) -> OrchestrationCommand {
    OrchestrationCommand::PermissionRequired {
        node_id: id(node),
        attempt,
        permission_id: permission_id.into(),
    }
}

fn resolved(node: &str, attempt: u32, permission_id: &str) -> OrchestrationCommand {
    OrchestrationCommand::PermissionResolved {
        node_id: id(node),
        attempt,
        permission_id: permission_id.into(),
        approved: true,
    }
}

#[test]
fn permission_requests_pause_the_step_until_every_one_is_resolved() {
    let (compiled, mut state) = started();
    run_step(&compiled, &mut state, "input", json!({}));
    admit(&compiled, &mut state, "execute", 1);

    apply(&compiled, &mut state, permission("execute", 1, "p1")).unwrap();
    apply(&compiled, &mut state, permission("execute", 1, "p2")).unwrap();
    assert_eq!(state.status, OrchestrationStatus::WaitingForPermission);
    assert_eq!(
        state.steps[&id("execute")].status,
        StepStatus::WaitingForPermission
    );
    assert_eq!(
        apply(&compiled, &mut state, permission("execute", 1, "p1"))
            .unwrap_err()
            .code,
        "duplicate_permission"
    );
    assert_eq!(
        apply(
            &compiled,
            &mut state,
            OrchestrationCommand::StepSucceeded {
                node_id: id("execute"),
                attempt: 1,
                output: report("completed"),
                details: AttemptDetails::default(),
            }
        )
        .unwrap_err()
        .code,
        "step_not_running",
        "a step cannot succeed while blocked on a permission"
    );

    apply(&compiled, &mut state, resolved("execute", 1, "p1")).unwrap();
    assert_eq!(state.status, OrchestrationStatus::WaitingForPermission);
    assert_eq!(
        apply(&compiled, &mut state, resolved("execute", 1, "p1"))
            .unwrap_err()
            .code,
        "unknown_permission"
    );
    apply(&compiled, &mut state, resolved("execute", 1, "p2")).unwrap();
    assert_eq!(state.status, OrchestrationStatus::Running);
    assert_eq!(state.steps[&id("execute")].status, StepStatus::Running);
}

#[test]
fn a_step_waiting_for_permission_can_fail_or_be_cancelled() {
    let (compiled, mut state) = started();
    run_step(&compiled, &mut state, "input", json!({}));
    admit(&compiled, &mut state, "execute", 1);
    apply(&compiled, &mut state, permission("execute", 1, "p1")).unwrap();
    let effects = apply(&compiled, &mut state, OrchestrationCommand::Cancel).unwrap();
    assert!(effects.contains(&OrchestrationEffect::CancelStep {
        node_id: id("execute"),
        attempt: 1
    }));
    assert!(state.steps[&id("execute")].pending_permissions.is_empty());

    let (compiled, mut state) = started();
    run_step(&compiled, &mut state, "input", json!({}));
    admit(&compiled, &mut state, "execute", 1);
    apply(&compiled, &mut state, permission("execute", 1, "p1")).unwrap();
    fail(
        &compiled,
        &mut state,
        "execute",
        1,
        OrchestrationError::new("step_timeout", "timed out"),
    );
    assert_eq!(state.status, OrchestrationStatus::Failed);
}

// ---------------------------------------------------------------------------
// Reducer: pause / resume
// ---------------------------------------------------------------------------

#[test]
fn pause_takes_effect_when_the_running_step_settles() {
    let (compiled, mut state) = started();
    admit(&compiled, &mut state, "input", 1);
    apply(&compiled, &mut state, OrchestrationCommand::Pause).unwrap();
    assert_eq!(
        state.status,
        OrchestrationStatus::Running,
        "in-flight work continues"
    );
    assert_eq!(
        apply(&compiled, &mut state, OrchestrationCommand::Pause)
            .unwrap_err()
            .code,
        "already_paused"
    );

    succeed(&compiled, &mut state, "input", 1, json!({}));
    assert_eq!(state.status, OrchestrationStatus::Paused);
    assert!(ready_steps(&compiled, &state).is_empty());
    assert_eq!(
        apply(
            &compiled,
            &mut state,
            OrchestrationCommand::StepAdmitted {
                node_id: id("execute"),
                attempt: 1
            }
        )
        .unwrap_err()
        .code,
        "run_not_ready"
    );

    apply(&compiled, &mut state, OrchestrationCommand::Resume).unwrap();
    assert_eq!(state.status, OrchestrationStatus::Ready);
    assert_eq!(ready_steps(&compiled, &state), vec![id("execute")]);
    assert_eq!(
        apply(&compiled, &mut state, OrchestrationCommand::Resume)
            .unwrap_err()
            .code,
        "not_paused"
    );
}

#[test]
fn created_runs_cannot_be_paused_but_can_be_cancelled() {
    let compiled = compiled_default();
    let mut state = OrchestrationRunState::new(OrchestrationRunId::from("r"), &compiled);
    assert!(apply(&compiled, &mut state, OrchestrationCommand::Pause).is_err());
    apply(&compiled, &mut state, OrchestrationCommand::Cancel).unwrap();
    assert_eq!(state.status, OrchestrationStatus::Cancelled);
}

// ---------------------------------------------------------------------------
// Reducer: budgets, abort
// ---------------------------------------------------------------------------

#[test]
fn usage_is_accumulated_and_exhausted_budgets_deny_admission() {
    let mut definition = default_orchestration_definition();
    definition.policies.max_tool_calls = Some(3);
    let compiled = compile(definition).unwrap();
    let mut state = started_with(&compiled);
    run_step(&compiled, &mut state, "input", json!({}));
    admit(&compiled, &mut state, "execute", 1);
    let effects = succeed_with(
        &compiled,
        &mut state,
        "execute",
        1,
        report("completed"),
        AttemptDetails {
            usage: UsageSummary {
                model_requests: 2,
                tool_calls: 3,
                tokens: 100,
                tokens_unknown: false,
                cost_usd: 0.0,
                cost_unknown: true,
            },
            ..AttemptDetails::default()
        },
    );
    assert!(matches!(
        events(&effects)[0],
        OrchestrationEvent::BudgetUpdated { usage } if usage.tool_calls == 3 && usage.cost_unknown
    ));

    let effects = apply(
        &compiled,
        &mut state,
        OrchestrationCommand::StepAdmitted {
            node_id: id("verify"),
            attempt: 1,
        },
    )
    .unwrap();
    assert_eq!(state.status, OrchestrationStatus::Failed);
    assert_eq!(
        state.error.as_ref().unwrap().code,
        error_codes::BUDGET_EXHAUSTED
    );
    assert!(effects
        .iter()
        .any(|effect| matches!(effect, OrchestrationEffect::Finish(result) if result.usage.tool_calls == 3)));
}

#[test]
fn attempt_budget_denies_retries() {
    let mut definition = default_orchestration_definition();
    definition.policies.max_total_attempts = 2;
    let compiled = compile(definition).unwrap();
    let mut state = started_with(&compiled);
    run_step(&compiled, &mut state, "input", json!({}));
    admit(&compiled, &mut state, "execute", 1);
    fail(
        &compiled,
        &mut state,
        "execute",
        1,
        OrchestrationError::retryable("timeout", "t", RetryReason::BackendTimeout),
    );
    assert_eq!(state.status, OrchestrationStatus::Failed);
    assert_eq!(
        state.error.as_ref().unwrap().code,
        error_codes::BUDGET_EXHAUSTED
    );
}

#[test]
fn abort_cancels_active_work_and_fails() {
    let (compiled, mut state) = started();
    admit(&compiled, &mut state, "input", 1);
    let effects = apply(
        &compiled,
        &mut state,
        OrchestrationCommand::Abort {
            error: OrchestrationError::new(error_codes::BUDGET_EXHAUSTED, "elapsed"),
        },
    )
    .unwrap();
    assert!(effects.contains(&OrchestrationEffect::CancelStep {
        node_id: id("input"),
        attempt: 1
    }));
    assert_eq!(state.status, OrchestrationStatus::Failed);
    assert_eq!(state.steps[&id("input")].status, StepStatus::Cancelled);
}

// ---------------------------------------------------------------------------
// Reducer: rejection of illegal transitions
// ---------------------------------------------------------------------------

#[test]
fn illegal_and_stale_transitions_are_rejected() {
    let (compiled, mut state) = started();
    let error = apply(
        &compiled,
        &mut state,
        OrchestrationCommand::StepAdmitted {
            node_id: id("execute"),
            attempt: 1,
        },
    )
    .expect_err("pending step cannot start");
    assert_eq!(error.code, "step_not_ready");

    let error = apply(
        &compiled,
        &mut state,
        OrchestrationCommand::StepAdmitted {
            node_id: id("input"),
            attempt: 2,
        },
    )
    .expect_err("attempt numbers are sequential");
    assert_eq!(error.code, "invalid_attempt");

    let error = apply(
        &compiled,
        &mut state,
        OrchestrationCommand::StepAdmitted {
            node_id: id("ghost"),
            attempt: 1,
        },
    )
    .expect_err("unknown step");
    assert_eq!(error.code, "unknown_step");

    admit(&compiled, &mut state, "input", 1);
    let error = apply(
        &compiled,
        &mut state,
        OrchestrationCommand::StepSucceeded {
            node_id: id("input"),
            attempt: 2,
            output: json!({}),
            details: AttemptDetails::default(),
        },
    )
    .expect_err("stale attempt rejected");
    assert_eq!(error.code, "stale_attempt");

    let error = apply(
        &compiled,
        &mut state,
        OrchestrationCommand::Start { input: json!({}) },
    )
    .expect_err("cannot start twice");
    assert_eq!(error.code, "invalid_run_status");

    let error = apply(
        &compiled,
        &mut state,
        OrchestrationCommand::RetryStep {
            node_id: id("input"),
        },
    )
    .expect_err("no retry scheduled");
    assert_eq!(error.code, "retry_not_scheduled");

    let error = apply(
        &compiled,
        &mut state,
        OrchestrationCommand::CancellationCompleted,
    )
    .expect_err("not cancelling");
    assert_eq!(error.code, "invalid_run_status");
}

#[test]
fn cancellation_emits_cancel_effect_for_running_step() {
    let (compiled, mut state) = started();
    admit(&compiled, &mut state, "input", 1);

    let effects = apply(&compiled, &mut state, OrchestrationCommand::Cancel).unwrap();
    assert_eq!(state.status, OrchestrationStatus::Cancelling);
    assert_eq!(state.steps[&id("input")].status, StepStatus::Cancelled);
    assert_eq!(state.steps[&id("execute")].status, StepStatus::Skipped);
    assert!(effects.contains(&OrchestrationEffect::CancelStep {
        node_id: id("input"),
        attempt: 1,
    }));
    assert_eq!(
        apply(&compiled, &mut state, OrchestrationCommand::Cancel)
            .unwrap_err()
            .code,
        "already_cancelling"
    );

    apply(
        &compiled,
        &mut state,
        OrchestrationCommand::CancellationCompleted,
    )
    .unwrap();
    assert_eq!(state.status, OrchestrationStatus::Cancelled);
}

#[test]
fn terminal_runs_reject_all_further_commands() {
    let (compiled, mut state) = started();
    apply(&compiled, &mut state, OrchestrationCommand::Cancel).unwrap();
    assert_eq!(state.status, OrchestrationStatus::Cancelled);
    for command in [
        OrchestrationCommand::Cancel,
        OrchestrationCommand::Pause,
        OrchestrationCommand::Resume,
        OrchestrationCommand::Recover,
        OrchestrationCommand::Start { input: json!({}) },
    ] {
        assert_eq!(
            apply(&compiled, &mut state, command).unwrap_err().code,
            "terminal_run"
        );
    }
}

// ---------------------------------------------------------------------------
// Reducer: restoration
// ---------------------------------------------------------------------------

fn round_trip(state: &OrchestrationRunState) -> OrchestrationRunState {
    serde_json::from_str(&serde_json::to_string(state).unwrap()).unwrap()
}

#[test]
fn recovery_keeps_completed_steps_and_fails_closed_on_in_flight_agent_work() {
    let (compiled, mut state) = started();
    run_step(
        &compiled,
        &mut state,
        "input",
        json!({ "request": "Build it" }),
    );
    admit(&compiled, &mut state, "execute", 1);

    let mut restored = round_trip(&state);
    apply(&compiled, &mut restored, OrchestrationCommand::Recover).unwrap();

    assert_eq!(restored.status, OrchestrationStatus::Failed);
    assert_eq!(
        restored.error.as_ref().unwrap().code,
        error_codes::INDETERMINATE_ATTEMPT
    );
    assert_eq!(restored.steps[&id("input")].status, StepStatus::Succeeded);
    assert_eq!(restored.steps[&id("input")].attempts.len(), 1, "not re-run");
}

#[test]
fn recovery_replays_interrupted_deterministic_steps() {
    let (compiled, mut state) = started();
    run_step(&compiled, &mut state, "input", json!({}));
    run_step(&compiled, &mut state, "execute", report("completed"));
    admit(&compiled, &mut state, "verify", 1);

    let mut restored = round_trip(&state);
    apply(&compiled, &mut restored, OrchestrationCommand::Recover).unwrap();

    assert_eq!(restored.status, OrchestrationStatus::Ready);
    assert_eq!(restored.steps[&id("execute")].attempts.len(), 1);
    assert_eq!(restored.steps[&id("verify")].status, StepStatus::Ready);
    assert_eq!(
        restored.steps[&id("verify")].attempts[0].status,
        AttemptStatus::Cancelled
    );
    admit(&compiled, &mut restored, "verify", 2);
}

#[test]
fn recovery_at_a_step_boundary_continues_where_it_left_off() {
    let (compiled, mut state) = started();
    run_step(&compiled, &mut state, "input", json!({}));
    run_step(&compiled, &mut state, "execute", report("completed"));
    let mut restored = round_trip(&state);
    apply(&compiled, &mut restored, OrchestrationCommand::Recover).unwrap();
    assert_eq!(restored.status, OrchestrationStatus::Ready);
    assert_eq!(ready_steps(&compiled, &restored), vec![id("verify")]);
}

#[test]
fn recovery_rejects_a_different_definition() {
    let (_, state) = started();
    let mut definition = default_orchestration_definition();
    definition.description = Some("edited in place".into());
    let edited = compile(definition).unwrap();
    let mut restored = round_trip(&state);
    assert_eq!(
        apply(&edited, &mut restored, OrchestrationCommand::Recover)
            .unwrap_err()
            .code,
        "definition_hash_mismatch"
    );
}
