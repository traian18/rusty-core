//! Acceptance criteria: judging the criteria a review reported, and telling a
//! producer sent back by a failed verification which of them to fix.
//!
//! Both read criteria the same way (status, id, which statuses defer to a
//! person, which plan criteria nobody reported), so those rules live here once.

use harness_core::orchestration::VerificationCheck;
use serde_json::{json, Value};

pub(super) struct JudgedCriteria {
    pub(super) passed: bool,
    pub(super) detail: String,
    /// `{id, how_to_test, evidence}` for every deferred criterion.
    pub(super) deferred: Vec<Value>,
}

impl JudgedCriteria {
    fn failed(detail: String) -> Self {
        Self {
            passed: false,
            detail,
            deferred: Vec::new(),
        }
    }
}

fn criterion_status(criterion: &Value) -> String {
    criterion["status"]
        .as_str()
        .unwrap_or("unverified")
        .trim()
        .to_lowercase()
}

fn criterion_id(criterion: &Value, index: usize) -> String {
    criterion["id"]
        .as_str()
        .filter(|id| !id.trim().is_empty())
        .map_or_else(|| format!("criterion {}", index + 1), str::to_owned)
}

/// Anything not explicitly deferred blocks, including statuses the check
/// does not name: an unknown status is not a pass.
fn is_deferred(status: &String, block_on: &[String], defer: &[String]) -> bool {
    !block_on.contains(status) && defer.contains(status)
}

/// Every criterion that did not pass, with its position and status.
fn unmet_criteria<'a>(
    criteria: impl IntoIterator<Item = &'a Value>,
) -> impl Iterator<Item = (usize, &'a Value, String)> {
    criteria
        .into_iter()
        .enumerate()
        .filter_map(|(index, criterion)| {
            let status = criterion_status(criterion);
            (status != "pass").then_some((index, criterion, status))
        })
}

/// The acceptance criterion ids of a task plan.
pub(super) fn plan_criteria(plan: Option<&Value>) -> Vec<String> {
    plan.and_then(|plan| plan["requirements"].as_array())
        .into_iter()
        .flatten()
        .flat_map(|requirement| requirement["criteria"].as_array().into_iter().flatten())
        .filter_map(|criterion| criterion["id"].as_str().map(str::to_owned))
        .collect()
}

/// The `expected` plan criterion ids no reported criterion answers.
fn missing_plan_criteria<'a>(expected: &'a [String], criteria: &[Value]) -> Vec<&'a str> {
    let wanted: Vec<&str> = expected.iter().map(String::as_str).collect();
    let found = match_results(&wanted, criteria, |c| c["id"].as_str());
    wanted
        .into_iter()
        .zip(found)
        .filter(|(_, result)| result.is_none())
        .map(|(id, _)| id)
        .collect()
}

/// Judges `value`, the reported criteria. With `expected` (a plan's criterion
/// ids) every one must also have been reported; ids match loosely.
pub(super) fn judge_criteria(
    value: Option<&Value>,
    block_on: &[String],
    defer: &[String],
    max_deferred: Option<u32>,
    expected: Option<&[String]>,
) -> JudgedCriteria {
    let Some(criteria) = value.and_then(Value::as_array) else {
        return JudgedCriteria::failed("criteria are missing or not an array".into());
    };
    if criteria.is_empty() {
        return JudgedCriteria::failed("no criteria were reported".into());
    }
    let mut blocking = Vec::new();
    let mut deferred = Vec::new();
    for (index, criterion, status) in unmet_criteria(criteria) {
        let id = criterion_id(criterion, index);
        if is_deferred(&status, block_on, defer) {
            deferred.push(manual_check(criterion, id));
        } else {
            blocking.push(format!("{id} [{status}]"));
        }
    }
    if let Some(expected) = expected {
        let missing = missing_plan_criteria(expected, criteria);
        blocking.extend(missing.into_iter().map(|id| format!("{id} [missing]")));
    }
    if let Some(max) = max_deferred.filter(|max| deferred.len() > *max as usize) {
        blocking.push(format!(
            "{} criteria were left for manual checks, at most {max} may be",
            deferred.len()
        ));
    }
    if !blocking.is_empty() {
        return JudgedCriteria::failed(format!("not met: {}", blocking.join(", ")));
    }
    let passed = criteria.len() - deferred.len();
    let detail = if deferred.is_empty() {
        format!("{passed} criteria passed")
    } else {
        format!(
            "{passed} criteria passed, {} left for manual checks",
            deferred.len()
        )
    };
    JudgedCriteria {
        passed: true,
        detail,
        deferred,
    }
}

/// The manual check a deferred criterion becomes.
fn manual_check(criterion: &Value, id: String) -> Value {
    let text = |field: &str| {
        criterion[field]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    json!({
        "id": id,
        "how_to_test": text("how_to_test").or(text("evidence")).unwrap_or("Check this by hand."),
        "evidence": text("evidence").unwrap_or(""),
    })
}

/// The fields of a `criteria` check that decide what a producer must fix.
struct CriteriaCheck<'a> {
    pointer: &'a str,
    plan_pointer: Option<&'a str>,
    block_on: &'a [String],
    defer: &'a [String],
}

fn criteria_checks(checks: &[VerificationCheck]) -> Vec<CriteriaCheck<'_>> {
    checks
        .iter()
        .filter_map(|check| match check {
            VerificationCheck::Criteria {
                pointer,
                plan_pointer,
                block_on,
                defer,
                ..
            } => Some(CriteriaCheck {
                pointer,
                plan_pointer: plan_pointer.as_deref(),
                block_on,
                defer,
            }),
            _ => None,
        })
        .collect()
}

/// What a producer sent back by a failed verification should fix. Criteria a
/// `criteria` check defers (only a person can check them) are listed apart so
/// the producer does not try to "fix" something that is not broken.
pub(super) fn verification_repair_focus(
    input: &Value,
    issues: &[String],
    checks: &[VerificationCheck],
) -> String {
    let mut unresolved = Vec::new();
    let mut manual = Vec::new();
    let criteria_checks = criteria_checks(checks);
    if criteria_checks.is_empty() {
        let criteria = input["check"]["criteria"].as_array().into_iter().flatten();
        unresolved.extend(
            unmet_criteria(criteria)
                .map(|(index, criterion, status)| focus_line(index, criterion, &status)),
        );
    }
    for check in &criteria_checks {
        sort_for_repair(input, check, &mut unresolved, &mut manual);
    }
    repair_message(input, issues, &unresolved, &manual)
}

/// Sort the criteria `check` reads into ones to fix and ones left to a person.
fn sort_for_repair(
    input: &Value,
    check: &CriteriaCheck<'_>,
    unresolved: &mut Vec<String>,
    manual: &mut Vec<String>,
) {
    let criteria = input.pointer(check.pointer).and_then(Value::as_array);
    // Plan criteria nobody reported are unmet too.
    if let Some(plan_pointer) = check.plan_pointer {
        let expected = plan_criteria(input.pointer(plan_pointer));
        let reported = criteria.map_or(&[][..], Vec::as_slice);
        unresolved.extend(
            missing_plan_criteria(&expected, reported)
                .into_iter()
                .map(|id| format!("- {id} [missing]: the review did not report it")),
        );
    }
    for (index, criterion, status) in unmet_criteria(criteria.into_iter().flatten()) {
        let line = focus_line(index, criterion, &status);
        if is_deferred(&status, check.block_on, check.defer) {
            manual.push(line);
        } else {
            unresolved.push(line);
        }
    }
}

fn focus_line(index: usize, criterion: &Value, status: &str) -> String {
    let evidence = criterion["evidence"]
        .as_str()
        .unwrap_or("No evidence supplied");
    format!(
        "- {} [{status}]: {evidence}",
        criterion_id(criterion, index)
    )
}

fn repair_message(
    input: &Value,
    issues: &[String],
    unresolved: &[String],
    manual: &[String],
) -> String {
    let mut parts = vec![format!("Verification did not pass: {}", issues.join("; "))];
    if !unresolved.is_empty() {
        parts.push(format!(
            "Focus on these unmet criteria:\n{}",
            unresolved.join("\n")
        ));
    }
    if !manual.is_empty() {
        parts.push(format!(
            "Left for the user to check by hand; do not try to fix these:\n{}",
            manual.join("\n")
        ));
    }
    if let Some(summary) = input["check"]["summary"].as_str() {
        parts.push(format!("Review summary: {summary}"));
    }
    parts.join("\n\n")
}

/// Which result answers which expected criterion. IDs are matched loosely
/// (`C-1`, `c1` and `C1` are one); when the results carry none of the expected
/// IDs (a reply that numbered them its own way) the same number of results
/// answers the criteria in order.
pub(crate) fn match_results<'a>(
    expected: &[&str],
    results: &'a [Value],
    id_of: impl Fn(&Value) -> Option<&str>,
) -> Vec<Option<&'a Value>> {
    let key = |id: &str| {
        id.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let by_id: Vec<Option<&Value>> = expected
        .iter()
        .map(|id| {
            results
                .iter()
                .find(|result| id_of(result).is_some_and(|found| key(found) == key(id)))
        })
        .collect();
    if by_id.iter().all(Option::is_none) && results.len() == expected.len() {
        return results.iter().map(Some).collect();
    }
    by_id
}

/// A missing or failed criterion must never be mistaken for acceptance.
pub(crate) fn requirement_coverage(
    plan: Option<&Value>,
    results: Option<&Value>,
) -> (bool, String) {
    let expected = plan_criteria(plan);
    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    let Some(results) = results.and_then(Value::as_array) else {
        return (false, "criterion results are missing".into());
    };
    let matched = match_results(&expected, results, |r| r["id"].as_str());
    let passed = !expected.is_empty()
        && matched.iter().all(|result| {
            result.is_some_and(|r| {
                r["status"] == "pass"
                    && r["evidence"].as_str().is_some_and(|s| !s.trim().is_empty())
            })
        });
    (
        passed,
        if passed {
            "all acceptance criteria have passing evidence"
        } else {
            "missing, failed or unevidenced acceptance criteria"
        }
        .into(),
    )
}

#[cfg(test)]
mod verification_feedback_tests {
    use super::*;

    #[test]
    fn repair_feedback_names_only_unmet_criteria_and_their_evidence() {
        let input = json!({"check": {
            "summary": "Native notifications still need work.",
            "criteria": [
                {"id": "C1", "status": "pass", "evidence": "Types exist."},
                {"id": "C2", "status": "fail", "evidence": "No native delivery in notifications.rs."},
                {"id": "C3", "status": "unverified", "evidence": "No question adapter test."}
            ]
        }});
        let focus = verification_repair_focus(&input, &["verdict was fail".into()], &[]);
        assert!(focus.contains("C2 [fail]: No native delivery in notifications.rs."));
        assert!(focus.contains("C3 [unverified]: No question adapter test."));
        assert!(!focus.contains("C1 [pass]"));
        assert!(focus.contains("Review summary: Native notifications still need work."));
    }

    fn criteria_check(max_deferred: Option<u32>) -> VerificationCheck {
        VerificationCheck::Criteria {
            pointer: "/check/criteria".into(),
            plan_pointer: None,
            block_on: vec!["fail".into()],
            defer: vec!["manual".into()],
            max_deferred,
        }
    }

    fn judge(criteria: Value, max_deferred: Option<u32>) -> JudgedCriteria {
        let VerificationCheck::Criteria {
            block_on, defer, ..
        } = criteria_check(max_deferred)
        else {
            unreachable!()
        };
        judge_criteria(Some(&criteria), &block_on, &defer, max_deferred, None)
    }

    #[test]
    fn manual_criteria_let_the_work_move_on_and_become_manual_checks() {
        let judged = judge(
            json!([
                {"id": "C1", "status": "pass", "evidence": "Unit test passes."},
                {"id": "C2", "status": "manual", "evidence": "Needs a phone.", "how_to_test": "Open the app on iOS and tap Share."}
            ]),
            None,
        );
        assert!(judged.passed, "{}", judged.detail);
        assert_eq!(judged.detail, "1 criteria passed, 1 left for manual checks");
        assert_eq!(
            judged.deferred,
            vec![
                json!({"id": "C2", "how_to_test": "Open the app on iOS and tap Share.", "evidence": "Needs a phone."})
            ]
        );
    }

    #[test]
    fn failed_unknown_or_missing_criteria_block() {
        let failed = judge(
            json!([{"id": "C1", "status": "fail", "evidence": "x"}]),
            None,
        );
        assert!(!failed.passed);
        assert_eq!(failed.detail, "not met: C1 [fail]");
        let unknown = judge(
            json!([{"id": "C1", "status": "unverified", "evidence": "x"}]),
            None,
        );
        assert!(!unknown.passed, "an undeclared status is not a pass");
        assert!(!judge(json!([]), None).passed, "no criteria is not a pass");
        assert!(!judge_criteria(None, &["fail".into()], &[], None, None).passed);
    }

    #[test]
    fn plan_criteria_nobody_reported_block_as_missing() {
        let plan = json!({"requirements": [{"id": "R1", "text": "t", "criteria": [{"id": "C-1", "text": "a"}, {"id": "C2", "text": "b"}]}]});
        let expected = plan_criteria(Some(&plan));
        assert_eq!(expected, ["C-1", "C2"]);
        let reported = json!([{"id": "c1", "status": "pass", "evidence": "x"}]);
        let block = ["fail".to_string()];
        let judged = judge_criteria(Some(&reported), &block, &[], None, Some(&expected));
        assert!(!judged.passed);
        assert_eq!(judged.detail, "not met: C2 [missing]");
        let all = json!([{"id": "C1", "status": "pass"}, {"id": "C2", "status": "pass"}]);
        assert!(judge_criteria(Some(&all), &block, &[], None, Some(&expected)).passed);
        let input = json!({"check": {"criteria": reported}, "plan": plan});
        let check = VerificationCheck::Criteria {
            pointer: "/check/criteria".into(),
            plan_pointer: Some("/plan".into()),
            block_on: block.to_vec(),
            defer: vec![],
            max_deferred: None,
        };
        let focus = verification_repair_focus(&input, &["criteria".into()], &[check]);
        assert!(focus.contains("C2 [missing]"), "{focus}");
    }

    #[test]
    fn deferring_more_than_allowed_blocks() {
        let criteria = json!([
            {"id": "C1", "status": "manual", "evidence": "a"},
            {"id": "C2", "status": "manual", "evidence": "b"}
        ]);
        assert!(judge(criteria.clone(), Some(2)).passed);
        let judged = judge(criteria, Some(1));
        assert!(!judged.passed);
        assert!(judged.detail.contains("at most 1"), "{}", judged.detail);
    }

    #[test]
    fn repair_feedback_keeps_manual_criteria_apart_from_unmet_ones() {
        let input = json!({"check": {"criteria": [
            {"id": "C1", "status": "fail", "evidence": "Button does nothing."},
            {"id": "C2", "status": "manual", "evidence": "Needs a printer."}
        ]}});
        let focus =
            verification_repair_focus(&input, &["criteria".into()], &[criteria_check(None)]);
        let (unmet, manual) = focus
            .split_once("Left for the user to check by hand")
            .expect("manual section");
        assert!(unmet.contains("C1 [fail]: Button does nothing."));
        assert!(!unmet.contains("C2"));
        assert!(manual.contains("C2 [manual]: Needs a printer."));
    }
}
