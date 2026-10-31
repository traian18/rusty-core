use super::unmet_criteria;

#[test]
fn unmet_criteria_come_from_the_repair_focus_and_skip_manual_checks() {
    let message = "Verification did not pass: criteria:/check/criteria: not met: C-2 [fail]\n\nFocus on these unmet criteria:\n- C-2 [fail]: Button does nothing.\n- c3 [unverified]: no test\n\nLeft for the user to check by hand; do not try to fix these:\n- C4 [manual]: needs a phone\n\nReview summary: close";
    let unmet = unmet_criteria(message);
    assert_eq!(
        unmet,
        ["c2".to_string(), "c3".to_string()].into_iter().collect()
    );
    let legacy = r#"rejected <criteria>[{"id":"C1","status":"fail"},{"id":"C2","status":"pass"}]</criteria>"#;
    assert_eq!(
        unmet_criteria(legacy),
        ["c1".to_string()].into_iter().collect()
    );
    assert!(unmet_criteria("no criteria here").is_empty());
}
