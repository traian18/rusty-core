//! Scores from findings: the worked example of the design, caps, the conditioned history cap,
//! the clamp, and the two dimensions.
use std::collections::BTreeMap;

use harness_code_health::{
    score_file,
    scoring::HISTORY_CATEGORY,
    Category, Finding, ScoringConfig,
    Severity::{self, *},
};

fn finding(id: &str, detector: &str, category: Category, severity: Severity) -> Finding {
    Finding {
        id: id.into(),
        detector: detector.into(),
        detector_version: 1,
        category,
        severity,
        path: "src/a.rs".into(),
        function: None,
        class: None,
        start_line: 1,
        end_line: 1,
        measured: BTreeMap::new(),
        reason: String::new(),
    }
}

fn complex(id: &str, severity: Severity) -> Finding {
    finding(id, "complex_method", Category::SizeAndComplexity, severity)
}
fn nested(id: &str, severity: Severity) -> Finding {
    finding(
        id,
        "nested_complexity",
        Category::StructuralComplexity,
        severity,
    )
}
fn entropy(id: &str, severity: Severity) -> Finding {
    finding(id, "change_entropy", Category::Organizational, severity)
}

#[test]
fn the_worked_example() {
    // CCN 16 (complex_method high: 1.2 × 1.21), nesting 5 (nested_complexity high: 1.2 × 1.34),
    // change entropy in the top tenth (high: 1.2 × 1.51), history cap 1 + 3.06.
    let findings = [complex("c", High), nested("n", High), entropy("e", High)];
    let score = score_file(&findings, &ScoringConfig::default());
    assert_eq!(score.defect.score, 5.13);
    assert_eq!(
        (score.structure_deduction, score.history_deduction),
        (3.06, 1.812)
    );
    assert_eq!(
        (score.impacts["c"], score.impacts["n"], score.impacts["e"]),
        (1.452, 1.608, 1.812)
    );
    let history = score.defect.categories[&HISTORY_CATEGORY];
    assert!(
        (history.cap - 3.5).abs() < 1e-9,
        "1 + 3.06 is above the category's own 3.5"
    );
    // Maintainability: code shape only, full weight: 1.2 + 1.2.
    assert_eq!(score.maintainability.score, 7.6);
}

#[test]
fn history_alone_costs_at_most_one_point() {
    let score = score_file(&[entropy("e", High)], &ScoringConfig::default());
    assert_eq!(score.defect.score, 9.0);
    assert_eq!(
        (score.structure_deduction, score.history_deduction),
        (0.0, 1.0)
    );
    assert_eq!(score.impacts["e"], 1.0);
    assert_eq!(
        score.maintainability.score, 10.0,
        "history is not maintainability"
    );
    let raw = score.defect.categories[&HISTORY_CATEGORY];
    assert!((raw.raw - 1.812).abs() < 1e-9 && raw.applied == 1.0);
}

#[test]
fn the_history_cap_grows_with_structure() {
    // A low bumpy road deducts 0.3 × 0.5 = 0.15, so history may deduct 1.15.
    let findings = [
        finding("b", "bumpy_road", Category::StructuralComplexity, Low),
        entropy("e", Critical),
    ];
    let score = score_file(&findings, &ScoringConfig::default());
    assert_eq!(
        (score.structure_deduction, score.history_deduction),
        (0.15, 1.15)
    );
    assert_eq!(score.defect.score, 8.7);
}

#[test]
fn a_category_over_its_cap_scales_every_finding_down_alike() {
    let findings = [complex("a", High), complex("b", High), complex("c", High)];
    let score = score_file(&findings, &ScoringConfig::default());
    // 3 × 1.452 = 4.356 capped at 1.5.
    assert_eq!(score.defect.score, 8.5);
    assert_eq!(
        [score.impacts["a"], score.impacts["b"], score.impacts["c"]],
        [0.5, 0.5, 0.5]
    );
    let size = score.defect.categories[&Category::SizeAndComplexity];
    assert!((size.raw - 4.356).abs() < 1e-9 && size.applied == 1.5);
    // Maintainability caps size at 2.0: 3 × 1.2 = 3.6 becomes 2.0.
    assert_eq!(score.maintainability.score, 8.0);
}

#[test]
fn the_worst_files_clamp_at_one_and_keep_their_depth() {
    let mut config = ScoringConfig::default();
    config
        .defect
        .caps
        .insert(Category::StructuralComplexity, 50.0);
    let findings: Vec<Finding> = (0..6).map(|i| nested(&format!("n{i}"), Critical)).collect();
    let score = score_file(&findings, &config);
    // 6 × 2.0 × 1.34 = 16.08.
    assert_eq!(score.defect.score, 1.0);
    assert_eq!(score.defect.unclamped, -6.08);
    assert_ne!(config.digest(), ScoringConfig::default().digest());
}

#[test]
fn findings_a_dimension_does_not_list_deduct_nothing_there() {
    let findings = [
        finding("x", "something_new", Category::SizeAndComplexity, Critical),
        complex("c", Medium),
    ];
    let score = score_file(&findings, &ScoringConfig::default());
    assert_eq!(score.impacts["x"], 0.0);
    // 0.7 × 1.21 = 0.847.
    assert_eq!(score.defect.score, 9.15);
    assert_eq!(score.maintainability.score, 9.3);
}

#[test]
fn maintainability_counts_large_methods_as_structure_and_never_moves_defect() {
    let large = finding("l", "large_method", Category::SizeAndComplexity, Critical);
    let alone = score_file(std::slice::from_ref(&large), &ScoringConfig::default());
    let mut config = ScoringConfig::default();
    config
        .maintainability
        .weights
        .insert("large_method".into(), 3.0);
    let heavier = score_file(&[large], &config);
    assert_eq!(
        alone.defect, heavier.defect,
        "maintainability tables never touch the defect score"
    );
    assert!(alone
        .maintainability
        .categories
        .contains_key(&Category::StructuralComplexity));
    assert!(heavier.maintainability.score < alone.maintainability.score);
}

#[test]
fn a_clean_file_scores_ten_and_scoring_is_deterministic() {
    let clean = score_file(&[], &ScoringConfig::default());
    assert_eq!(
        (
            clean.defect.score,
            clean.maintainability.score,
            clean.structure_deduction
        ),
        (10.0, 10.0, 0.0)
    );
    let findings = [complex("c", High), nested("n", High), entropy("e", High)];
    assert_eq!(
        score_file(&findings, &ScoringConfig::default()),
        score_file(&findings, &ScoringConfig::default())
    );
}
