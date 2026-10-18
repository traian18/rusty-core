//! From source text to findings: measurements from the code-analysis crate, judged here.
use harness_code_analysis::{metrics::measure_file, LanguageId};
use harness_code_health::{
    detect_file, score_file, DetectorConfig, FileContext, ScoringConfig, Severity,
};

#[test]
fn a_tangled_rust_function_is_reported_for_nesting_conditions_and_complexity() {
    let source = r#"
pub fn route(a: u32, b: u32, c: bool, d: bool, e: &str) -> u32 {
    if c && (d || !c) && a > b {
        for i in 0..a {
            if i % 2 == 0 {
                while b > i {
                    if d {
                        return 1;
                    }
                }
            }
        }
    }
    match e {
        "x" => 1,
        "y" => 2,
        "z" => 3,
        _ => 0,
    }
}
"#;
    let metrics = measure_file(&LanguageId::Rust, "src/route.rs", source.as_bytes()).unwrap();
    let context = FileContext::new("src/route.rs", &metrics).with_importers(2, None);
    let findings = detect_file(&context, &DetectorConfig::default());
    let by: Vec<(&str, Severity)> = findings
        .iter()
        .map(|f| (f.detector.as_str(), f.severity))
        .collect();
    // Nesting: if > for > if > while > if = 5. Condition: three operators, mixed, negated.
    // CCN: 1 + if, &&, ||, &&, for, if, while, if, three arms = 12.
    assert!(
        by.contains(&("nested_complexity", Severity::High)),
        "{by:?}"
    );
    assert!(
        by.contains(&("complex_conditional", Severity::Low)),
        "{by:?}"
    );
    assert!(by.contains(&("complex_method", Severity::Medium)), "{by:?}");
    assert!(
        !by.iter().any(|(d, _)| *d == "brain_method"),
        "only two importers"
    );
    let ccn = findings
        .iter()
        .find(|f| f.detector == "complex_method")
        .unwrap()
        .measured["ccn"];
    assert_eq!(ccn, 12);
    // Nesting high (1.2 × 1.34) and the low condition (0.3 × 1.33) are structure; CCN 12 is a
    // medium complex method (0.7 × 1.21): 10 − 2.854.
    let score = score_file(&findings, &ScoringConfig::default());
    assert_eq!(findings.len(), 3, "{by:?}");
    assert_eq!(score.defect.score, 7.15);
    assert_eq!(score.history_deduction, 0.0);
}

#[test]
fn a_clean_file_has_no_findings() {
    let source = b"def add(a, b):\n    return a + b\n\n\nclass Point:\n    def __init__(self, x, y):\n        self.x = x\n        self.y = y\n";
    let metrics = measure_file(&LanguageId::Python, "p.py", source).unwrap();
    let context = FileContext::new("p.py", &metrics).with_importers(50, Some(4));
    assert!(detect_file(&context, &DetectorConfig::default()).is_empty());
}
