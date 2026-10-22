//! The deterministic `verify` node: runs each check against the bound input
//! and turns the evidence into a report or a retryable failure.

use harness_core::orchestration::{
    Evidence, OrchestrationError, OrchestrationNode, OrchestrationNodeId, OrchestrationRunState,
    OutputBinding, RetryReason, VerificationCheck, VerifyNodeConfig,
};
use serde_json::{json, Value};

use super::criteria::{
    judge_criteria, plan_criteria, requirement_coverage, verification_repair_focus,
};
use super::{resolve_inputs, ArtifactResolver, Validation};

type Producers<'a> = &'a (dyn Fn(&OrchestrationNodeId) -> Option<Value> + Sync);

/// What every check reads.
struct CheckInputs<'a> {
    node: &'a OrchestrationNode,
    input: &'a Value,
    producers: Producers<'a>,
    validation: &'a Validation<'a>,
    artifacts: &'a dyn ArtifactResolver,
}

/// Runs every check (not just until the first failure) so the recorded
/// evidence and the feedback given to a retried producer are complete.
///
/// Returns the verification report as the step output, or a retryable
/// `verification_failed` error. Evidence is returned either way.
pub(crate) async fn execute_verify(
    state: &OrchestrationRunState,
    node: &OrchestrationNode,
    config: &VerifyNodeConfig,
    producers: Producers<'_>,
    validation: &Validation<'_>,
    artifacts: &dyn ArtifactResolver,
) -> (Result<Value, OrchestrationError>, Vec<Evidence>) {
    let input = match resolve_inputs(state, &node.input_bindings) {
        Ok(input) => input,
        Err(error) => return (Err(error), Vec::new()),
    };
    let inputs = CheckInputs {
        node,
        input: &input,
        producers,
        validation,
        artifacts,
    };
    let mut evidence = Vec::new();
    let mut manual_checks = Vec::new();
    for check in &config.checks {
        let (passed, detail) = run_check(check, &inputs, &mut manual_checks).await;
        evidence.push(Evidence {
            check: check.id(),
            passed,
            detail,
        });
    }

    let issues: Vec<_> = evidence
        .iter()
        .filter(|item| !item.passed)
        .map(|item| format!("{}: {}", item.check, item.detail))
        .collect();
    let report = verification_report(&evidence, &issues, manual_checks, &config.checks);
    let result = if issues.is_empty() {
        Ok(report)
    } else {
        Err(verification_failure(&input, &issues, &config.checks))
    };
    (result, evidence)
}

/// Run one check: whether it passed and what it found. Deferred criteria are
/// added to `manual_checks`.
async fn run_check(
    check: &VerificationCheck,
    inputs: &CheckInputs<'_>,
    manual_checks: &mut Vec<Value>,
) -> (bool, String) {
    let input = inputs.input;
    match check {
        VerificationCheck::Criteria {
            pointer,
            plan_pointer,
            block_on,
            defer,
            max_deferred,
        } => {
            let expected = plan_pointer
                .as_ref()
                .map(|plan_pointer| plan_criteria(input.pointer(plan_pointer)));
            let judged = judge_criteria(
                input.pointer(pointer),
                block_on,
                defer,
                *max_deferred,
                expected.as_deref(),
            );
            manual_checks.extend(judged.deferred);
            (judged.passed, judged.detail)
        }
        VerificationCheck::RequirementsSatisfied {
            plan_pointer,
            results_pointer,
        } => requirement_coverage(input.pointer(plan_pointer), input.pointer(results_pointer)),
        VerificationCheck::Schema => check_producer_schemas(inputs),
        VerificationCheck::RequiredStatus { pointer, equals } => {
            check_required_status(input, pointer, equals)
        }
        VerificationCheck::ArtifactExists { pointer } => check_artifact_exists(input, pointer),
        VerificationCheck::ArtifactsResolvable { pointer } => {
            check_artifacts_resolvable(input, pointer, inputs.artifacts).await
        }
    }
}

/// Every bound producer output matches its producer's schema.
fn check_producer_schemas(inputs: &CheckInputs<'_>) -> (bool, String) {
    let mut issues = Vec::new();
    for binding in &inputs.node.input_bindings {
        let OutputBinding::NodeOutput { node_id, .. } = &binding.source else {
            continue;
        };
        let Some(schema) = (inputs.producers)(node_id) else {
            continue;
        };
        let value = inputs.input.get(&binding.target).unwrap_or(&Value::Null);
        if let Err(error) = inputs.validation.validator.validate(&schema, value) {
            issues.push(format!("{}: {error}", binding.target));
        }
    }
    if issues.is_empty() {
        (true, "bound outputs match their producers' schemas".into())
    } else {
        (false, issues.join("; "))
    }
}

fn check_required_status(input: &Value, pointer: &str, equals: &str) -> (bool, String) {
    match input.pointer(pointer).and_then(Value::as_str) {
        Some(actual) if actual == equals => (true, format!("{pointer} == {equals}")),
        Some(actual) => (
            false,
            format!("{pointer} is {actual:?}, expected {equals:?}"),
        ),
        None => (false, format!("{pointer} is missing or not a string")),
    }
}

fn check_artifact_exists(input: &Value, pointer: &str) -> (bool, String) {
    let exists = match input.pointer(pointer) {
        None | Some(Value::Null) => false,
        Some(Value::String(value)) => !value.is_empty(),
        Some(Value::Array(value)) => !value.is_empty(),
        Some(Value::Object(value)) => !value.is_empty(),
        Some(_) => true,
    };
    if exists {
        (true, format!("{pointer} is present"))
    } else {
        (false, format!("{pointer} is missing or empty"))
    }
}

async fn check_artifacts_resolvable(
    input: &Value,
    pointer: &str,
    artifacts: &dyn ArtifactResolver,
) -> (bool, String) {
    let Some(items) = input.pointer(pointer).and_then(Value::as_array) else {
        return (false, format!("{pointer} is missing or not an array"));
    };
    let mut problems = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if let Err(problem) = resolve_artifact(item, artifacts).await {
            problems.push(format!("[{index}] {problem}"));
        }
    }
    if problems.is_empty() {
        (true, format!("{} artifact(s) resolved", items.len()))
    } else {
        (false, problems.join("; "))
    }
}

async fn resolve_artifact(item: &Value, artifacts: &dyn ArtifactResolver) -> Result<(), String> {
    let kind = item.get("kind").and_then(Value::as_str);
    let reference = item.get("reference").and_then(Value::as_str);
    match (kind, reference) {
        (Some(kind), Some(reference)) => artifacts.resolve(kind, reference).await,
        _ => Err("artifact needs string kind and reference".into()),
    }
}

fn verification_report(
    evidence: &[Evidence],
    issues: &[String],
    manual_checks: Vec<Value>,
    checks: &[VerificationCheck],
) -> Value {
    let mut report = json!({
        "passed": issues.is_empty(),
        "checks": evidence.iter().map(|item| json!({
            "id": item.check,
            "passed": item.passed,
            "detail": item.detail,
        })).collect::<Vec<_>>(),
        "issues": issues,
    });
    // Always present when criteria are judged, so a later step can bind to it.
    if checks
        .iter()
        .any(|check| matches!(check, VerificationCheck::Criteria { .. }))
    {
        report["manual_checks"] = Value::Array(manual_checks);
    }
    report
}

/// A review blocked by its environment or by the user cannot be repaired by
/// retrying the producer; anything else can.
fn verification_failure(
    input: &Value,
    issues: &[String],
    checks: &[VerificationCheck],
) -> OrchestrationError {
    if is_blocked_verdict(input) {
        return OrchestrationError::new(
            "verification_blocked",
            input["check"]["summary"]
                .as_str()
                .unwrap_or("Verification blocked"),
        );
    }
    OrchestrationError::retryable(
        "verification_failed",
        verification_repair_focus(input, issues, checks),
        RetryReason::VerificationFailed,
    )
}

fn is_blocked_verdict(input: &Value) -> bool {
    input
        .get("check")
        .and_then(|v| v.get("verdict"))
        .and_then(Value::as_str)
        .is_some_and(|v| v.starts_with("blocked_environment:") || v.starts_with("blocked_user:"))
}
