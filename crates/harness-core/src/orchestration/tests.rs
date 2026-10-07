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

#[test]
fn workflow_budgets_are_optional_but_explicit_values_are_enforced() {
    let policies: OrchestrationPolicies = serde_json::from_value(json!({})).unwrap();
    assert_eq!(policies.max_steps, None);
    assert_eq!(policies.max_total_attempts, None);
    let mut definition = default_orchestration_definition();
    definition.policies = policies;
    assert!(compile(definition.clone()).is_ok());
    definition.policies.max_steps = Some(1);
    assert!(compile(definition.clone()).is_err());
    definition.policies.max_steps = None;
    definition.policies.max_total_attempts = Some(0);
    assert!(compile(definition).is_err());
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
fn compiler_validates_criteria_checks() {
    let with_check = |block_on: &[&str], defer: &[&str]| {
        let mut definition = default_orchestration_definition();
        for node in &mut definition.nodes {
            if let OrchestrationNodeKind::Verify(config) = &mut node.kind {
                config.checks = vec![VerificationCheck::Criteria {
                    pointer: "/report/criteria".into(),
                    plan_pointer: None,
                    block_on: block_on.iter().map(|s| s.to_string()).collect(),
                    defer: defer.iter().map(|s| s.to_string()).collect(),
                    max_deferred: None,
                }];
            }
        }
        compile(definition)
    };
    with_check(&["fail"], &["manual"]).expect("valid criteria check");
    for (block_on, defer) in [
        (&[][..], &["manual"][..]),
        (&["fail"], &["fail"]),
        (&["fail"], &["pass"]),
        (&["fail", ""], &[]),
    ] {
        let error = with_check(block_on, defer).expect_err("invalid criteria check");
        assert!(
            codes(&error).contains(&"invalid_criteria_check"),
            "{block_on:?} / {defer:?}"
        );
    }
    let parsed: VerificationCheck =
        serde_json::from_value(json!({"type": "criteria", "pointer": "/check/criteria"})).unwrap();
    assert_eq!(
        parsed,
        VerificationCheck::Criteria {
            pointer: "/check/criteria".into(),
            plan_pointer: None,
            block_on: vec!["fail".into()],
            defer: vec!["manual".into()],
            max_deferred: None,
        },
        "fail blocks and manual defers by default"
    );
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
    definition.policies.max_total_attempts = Some(2);
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

#[test]
fn an_output_contract_is_optional() {
    let mut value = serde_json::to_value(default_orchestration_definition()).unwrap();
    value.as_object_mut().unwrap().remove("output_contract");
    let definition: OrchestrationDefinition = serde_json::from_value(value).unwrap();
    assert!(definition.output_contract.source.is_none());
    assert!(!definition.output_contract.strict);
    compile(definition).expect("compiles without a contract");
}

// ---------------------------------------------------------------------------
// Approval and questions to the user

/// input → plan → approve → build → output.
fn approval_definition(approval: Value) -> OrchestrationDefinition {
    let mut config = json!({"subject": {"type": "node_output", "node_id": "plan", "pointer": ""}});
    for (key, value) in approval.as_object().unwrap() {
        config[key] = value.clone();
    }
    let agent = |name: &str| {
        json!({"id": name, "name": name, "type": "agent",
            "config": {"instructions": "work", "structured_output": "text"},
            "retry": {"max_attempts": 2, "retry_on": ["backend_rate_limited"]}})
    };
    serde_json::from_value(json!({
        "schema_version": 1, "id": "approve.flow", "revision": 1, "name": "Approve",
        "nodes": [
            {"id": "input", "name": "Input", "type": "input", "config": {}},
            agent("plan"),
            {"id": "approve", "name": "Approve", "type": "approval", "config": config},
            agent("build"),
            {"id": "output", "name": "Output", "type": "output",
             "config": {"source": {"type": "node_output", "node_id": "build", "pointer": ""}, "strict": false}}
        ],
        "edges": [
            {"id": "e1", "source": "input", "target": "plan", "condition": "on_success"},
            {"id": "e2", "source": "plan", "target": "approve", "condition": "on_success"},
            {"id": "e3", "source": "approve", "target": "build", "condition": "on_success"},
            {"id": "e4", "source": "build", "target": "output", "condition": "on_success"}
        ]
    }))
    .unwrap()
}

fn question() -> InputRequest {
    InputRequest {
        id: "approve:1".into(),
        kind: "approval".into(),
        prompt: "Review the plan".into(),
        subject: json!("the plan"),
        decisions: vec![
            InputDecision {
                id: "approve".into(),
                label: "Approve".into(),
                requires_text: false,
            },
            InputDecision {
                id: "request_changes".into(),
                label: "Request changes".into(),
                requires_text: true,
            },
        ],
    }
}

/// A run whose approval step has asked `question()`.
fn asking(approval: Value) -> (CompiledOrchestration, OrchestrationRunState) {
    let compiled = compile(approval_definition(approval)).expect("approval flow compiles");
    let mut state = started_with(&compiled);
    run_step(&compiled, &mut state, "input", json!({"request": "x"}));
    run_step(&compiled, &mut state, "plan", json!("the plan"));
    admit(&compiled, &mut state, "approve", 1);
    let effects = apply(
        &compiled,
        &mut state,
        OrchestrationCommand::InputRequired {
            node_id: id("approve"),
            attempt: 1,
            request: question(),
        },
    )
    .unwrap();
    assert!(matches!(
        events(&effects)[..],
        [OrchestrationEvent::InputRequested { .. }]
    ));
    (compiled, state)
}

fn resolve(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    request_id: &str,
    decision: &str,
    text: Option<&str>,
) -> Result<Vec<OrchestrationEffect>, TransitionError> {
    apply(
        compiled,
        state,
        OrchestrationCommand::InputResolved {
            node_id: id("approve"),
            attempt: 1,
            request_id: request_id.into(),
            response: InputResponse {
                decision: decision.into(),
                text: text.map(str::to_owned),
                by: Responder::User,
            },
        },
    )
}

#[test]
fn a_question_puts_the_run_in_waiting_for_input_until_answered() {
    let (compiled, mut state) = asking(json!({}));
    assert_eq!(state.status, OrchestrationStatus::WaitingForInput);
    assert_eq!(
        state.steps[&id("approve")].status,
        StepStatus::WaitingForInput
    );
    assert_eq!(state.steps[&id("approve")].pending_input, Some(question()));
    let wrong = |state: &mut OrchestrationRunState, id: &str, decision: &str, text| {
        resolve(&compiled, state, id, decision, text)
            .expect_err("rejected")
            .code
    };
    assert_eq!(wrong(&mut state, "other", "approve", None), "unknown_input");
    assert_eq!(
        wrong(&mut state, "approve:1", "maybe", None),
        "invalid_decision"
    );
    assert_eq!(
        wrong(&mut state, "approve:1", "request_changes", Some(" ")),
        "missing_text"
    );
    assert_eq!(state.status, OrchestrationStatus::WaitingForInput);
    resolve(&compiled, &mut state, "approve:1", "approve", None).unwrap();
    assert_eq!(state.status, OrchestrationStatus::Running);
    assert!(state.steps[&id("approve")].pending_input.is_none());
}

#[test]
fn an_unanswered_approval_is_asked_again_after_a_restart() {
    let (compiled, mut state) = asking(json!({}));
    let effects = apply(&compiled, &mut state, OrchestrationCommand::Recover).unwrap();
    assert!(!state.status.is_terminal(), "{:?}", state.error);
    let step = &state.steps[&id("approve")];
    assert_eq!(step.status, StepStatus::Ready);
    assert!(step.pending_input.is_none());
    assert!(events(&effects).contains(&&OrchestrationEvent::StepReady {
        node_id: id("approve")
    }));
}

#[test]
fn requested_changes_go_back_with_a_fresh_retry_budget() {
    let (compiled, mut state) = asking(json!({}));
    resolve(
        &compiled,
        &mut state,
        "approve:1",
        "request_changes",
        Some("more"),
    )
    .unwrap();
    let effects = fail(
        &compiled,
        &mut state,
        "approve",
        1,
        OrchestrationError::retryable("changes_requested", "more", RetryReason::ChangesRequested),
    );
    assert!(
        events(&effects).contains(&&OrchestrationEvent::StepRetryScheduled {
            node_id: id("plan"),
            next_attempt: 2,
            triggered_by: id("approve"),
        })
    );
    let plan = &state.steps[&id("plan")];
    assert_eq!(plan.status, StepStatus::RetryScheduled);
    assert_eq!(plan.retry_base, 1);
    assert_eq!(plan.feedback.last().unwrap().message, "more");
    assert_eq!(state.steps[&id("approve")].status, StepStatus::Pending);

    // The revised plan may still be retried after a rate limit.
    apply(
        &compiled,
        &mut state,
        OrchestrationCommand::RetryStep {
            node_id: id("plan"),
        },
    )
    .unwrap();
    admit(&compiled, &mut state, "plan", 2);
    fail(
        &compiled,
        &mut state,
        "plan",
        2,
        OrchestrationError::retryable(
            "BACKEND_ERROR",
            "RateLimited",
            RetryReason::BackendRateLimited,
        ),
    );
    assert_eq!(state.steps[&id("plan")].status, StepStatus::RetryScheduled);
}

#[test]
fn changes_past_the_revision_limit_end_the_run() {
    let (compiled, mut state) = asking(json!({"max_revisions": 0}));
    fail(
        &compiled,
        &mut state,
        "approve",
        1,
        OrchestrationError::retryable("changes_requested", "more", RetryReason::ChangesRequested),
    );
    assert_eq!(state.status, OrchestrationStatus::Failed);
}

#[test]
fn compiler_validates_approvals() {
    let issues = |approval: Value| {
        compile(approval_definition(approval))
            .err()
            .map(|error| codes(&error))
            .unwrap_or_default()
    };
    assert!(
        issues(json!({})).is_empty(),
        "revise targets need no retry_on entry"
    );
    assert!(
        issues(json!({"subject": {"type": "node_output", "node_id": "build", "pointer": ""}}))
            .contains(&"unavailable_binding_source")
    );
    assert!(issues(json!({"revise_target": "build"})).contains(&"invalid_retry_target"));
    assert!(issues(json!({"revise_target": "input"})).contains(&"invalid_retry_target"));
    assert!(issues(json!({"revise_target": "nope"})).contains(&"unknown_retry_target"));
    assert!(issues(json!({"max_revisions": 11})).contains(&"invalid_max_revisions"));
    assert!(issues(json!({"skip_if_empty": "items"})).contains(&"invalid_pointer"));
    let compiled = compile(approval_definition(json!({}))).unwrap();
    assert_eq!(
        compiled.retry_spans[&id("approve")],
        vec![id("plan"), id("approve")]
    );
}

// ---------------------------------------------------------------------------
// Task queues

/// input → plan → build (a task queue over the plan) → output.
fn queue_definition(plan: Value, plan_pointer: &str) -> OrchestrationDefinition {
    serde_json::from_value(json!({
        "schema_version": 1, "id": "queue.flow", "revision": 1, "name": "Queue",
        "nodes": [
            {"id": "input", "name": "Input", "type": "input", "config": {}},
            plan,
            {"id": "build", "name": "Build", "type": "agent",
             "config": {"instructions": "build", "structured_output": "text",
                        "task_queue": {"plan_pointer": plan_pointer, "review_profile": {"id": "review"},
                                       "review_instructions": "review", "max_repairs": 1}},
             "input_bindings": [{"target": "plan", "source": {"type": "node_output", "node_id": "plan", "pointer": ""}}]},
            {"id": "output", "name": "Output", "type": "output",
             "config": {"source": {"type": "node_output", "node_id": "build", "pointer": ""}, "strict": false}}
        ],
        "edges": [
            {"id": "e1", "source": "input", "target": "plan", "condition": "on_success"},
            {"id": "e2", "source": "plan", "target": "build", "condition": "on_success"},
            {"id": "e3", "source": "build", "target": "output", "condition": "on_success"}
        ]
    }))
    .unwrap()
}

fn plan_step(structured_output: &str, schema: Value) -> Value {
    json!({"id": "plan", "name": "Plan", "type": "agent",
           "config": {"instructions": "plan", "structured_output": structured_output},
           "output_schema": schema})
}

fn registered_plan() -> Value {
    json!({"type": "registry", "schema_id": TASK_PLAN_SCHEMA_ID, "revision": TASK_PLAN_SCHEMA_REVISION})
}

#[test]
fn a_task_queue_needs_a_step_that_writes_a_task_plan() {
    let queue: TaskQueueConfig = serde_json::from_value(json!({
        "plan_pointer": "/plan", "review_profile": {"id": "review"}, "review_instructions": "r", "max_repairs": 1
    }))
    .unwrap();
    assert_eq!(
        queue.on_task_failure,
        TaskFailurePolicy::Ask,
        "asks by default"
    );
    assert_eq!(queue.plan_binding(), Some("plan"));

    let compiled = compile(queue_definition(
        plan_step("host_validated", registered_plan()),
        "/plan",
    ))
    .expect("a typed plan compiles");
    assert_eq!(
        compiled.retry_spans[&id("build")],
        vec![id("plan"), id("build")]
    );
    let inline = json!({"type": "inline", "name": "plan", "schema": task_plan_schema()});
    assert!(compile(queue_definition(
        plan_step("host_validated", inline),
        "/plan"
    ))
    .is_ok());

    let text_plan = plan_step(
        "text",
        json!({"type": "inline", "name": "plan", "schema": {"type": "string"}}),
    );
    let error = compile(queue_definition(text_plan, "/plan")).expect_err("a text plan");
    let issue = error
        .issues
        .iter()
        .find(|issue| issue.code == "task_plan_source")
        .unwrap();
    assert!(
        issue.message.contains(TASK_PLAN_SCHEMA_ID),
        "{}",
        issue.message
    );

    let missing = compile(queue_definition(
        plan_step("host_validated", registered_plan()),
        "/tasks",
    ));
    assert!(codes(&missing.unwrap_err()).contains(&"task_plan_source"));
}

#[test]
fn asking_to_revise_the_plan_reruns_the_planner_and_drops_the_queue_progress() {
    let compiled = compile(queue_definition(
        plan_step("host_validated", registered_plan()),
        "/plan",
    ))
    .unwrap();
    let mut state = started_with(&compiled);
    run_step(&compiled, &mut state, "input", json!({"request": "x"}));
    run_step(&compiled, &mut state, "plan", json!({"status": "ready"}));
    admit(&compiled, &mut state, "build", 1);
    apply(
        &compiled,
        &mut state,
        OrchestrationCommand::RecordCheckpoint {
            node_id: id("build"),
            attempt: 1,
            value: json!({"completed": [{"task_id": "T1"}]}),
        },
    )
    .unwrap();
    fail(
        &compiled,
        &mut state,
        "build",
        1,
        OrchestrationError::retryable(
            "changes_requested",
            "split T2",
            RetryReason::ChangesRequested,
        ),
    );
    let plan = &state.steps[&id("plan")];
    assert_eq!(plan.status, StepStatus::RetryScheduled);
    assert_eq!(plan.feedback.last().unwrap().message, "split T2");
    assert_eq!(plan.retry_base, 1);
    let build = &state.steps[&id("build")];
    assert_eq!(build.status, StepStatus::Pending);
    assert!(build.checkpoint.is_none(), "a new plan starts a new queue");
}

#[test]
fn the_registered_task_plan_schema_uses_only_supported_keywords() {
    let mut definition = queue_definition(plan_step("host_validated", registered_plan()), "/plan");
    definition.output_contract.schema = SchemaReference::Inline {
        name: "plan".into(),
        schema: task_plan_schema(),
    };
    compile(definition).expect("every keyword of the task plan schema is supported");
}

// ---------------------------------------------------------------------------
// Subflows

fn subflow_definition(target: Value) -> OrchestrationDefinition {
    serde_json::from_value(json!({
        "schema_version": 1, "id": "sub.flow", "revision": 1, "name": "Sub",
        "nodes": [
            {"id": "input", "name": "Input", "type": "input", "config": {}},
            {"id": "sub", "name": "Sub", "type": "subflow", "config": {"target": target}},
            {"id": "output", "name": "Output", "type": "output",
             "config": {"source": {"type": "node_output", "node_id": "sub", "pointer": ""}, "strict": false}}
        ],
        "edges": [
            {"id": "e1", "source": "input", "target": "sub", "condition": "on_success"},
            {"id": "e2", "source": "sub", "target": "output", "condition": "on_success"}
        ]
    }))
    .unwrap()
}

#[test]
fn subflow_targets_are_checked_and_steps_become_one_step_flows() {
    for target in [
        json!({"type": "flow", "id": "investigate"}),
        json!({"type": "flow", "id": "investigate", "revision": 3}),
        json!({"type": "step", "instructions": "Look into it.", "profile": {"id": "research"}}),
    ] {
        compile(subflow_definition(target)).expect("valid target");
    }
    for target in [
        json!({"type": "flow", "id": "  "}),
        json!({"type": "step", "instructions": " "}),
    ] {
        let error = compile(subflow_definition(target)).expect_err("invalid target");
        assert!(codes(&error).contains(&"invalid_subflow"));
    }
    let target: SubflowTarget =
        serde_json::from_value(json!({"type": "step", "instructions": "Look."})).unwrap();
    let one_step = target
        .step_definition(&id("sub"), &["request".into(), "context".into()])
        .expect("a step is a flow");
    let compiled = compile(one_step).expect("the one-step flow compiles");
    let step = &compiled.nodes[&id("step")];
    assert_eq!(
        step.input_bindings
            .iter()
            .map(|b| b.target.as_str())
            .collect::<Vec<_>>(),
        ["request", "context"]
    );
    assert!(
        matches!(&step.kind, OrchestrationNodeKind::Agent(config) if config.tools == ToolScope::Inherit)
    );
    assert!(SubflowTarget::Flow {
        id: "x".into(),
        revision: None
    }
    .step_definition(&id("sub"), &[])
    .is_none());
}

#[test]
fn task_queue_flows_are_checked() {
    let with_flows = |flows: Value| {
        let mut definition =
            queue_definition(plan_step("host_validated", registered_plan()), "/plan");
        let mut value = serde_json::to_value(&definition).unwrap();
        value["nodes"][2]["config"]["task_queue"]["flows"] = flows;
        definition = serde_json::from_value(value).unwrap();
        compile(definition)
    };
    with_flows(json!({"research": {"type": "flow", "id": "investigate"}})).expect("valid flows");
    for flows in [
        json!({"": {"type": "flow", "id": "investigate"}}),
        json!({"research": {"type": "flow", "id": ""}}),
        json!({"research": {"type": "step", "instructions": ""}}),
    ] {
        assert!(codes(&with_flows(flows).unwrap_err()).contains(&"invalid_subflow"));
    }
}
