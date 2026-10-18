//! Every detector at every severity edge, on measurements built by hand.
mod support;

use harness_code_health::{registry, Category, Severity::*};
use support::*;

#[test]
fn nested_complexity_steps() {
    let at = |depth| severity_of("nested_complexity", |f| f.max_nesting = depth);
    assert_eq!(
        [at(3), at(4), at(5), at(6), at(7)],
        [None, Some(Medium), Some(High), Some(High), Some(Critical)]
    );
}

#[test]
fn complex_conditional_steps_and_flat_chains() {
    let at = |ops, mixed, negated| {
        severity_of("complex_conditional", |f| {
            f.conditions = vec![condition(ops, mixed, negated)]
        })
    };
    assert_eq!(
        [
            at(2, true, false),
            at(3, true, false),
            at(4, true, false),
            at(5, true, false),
            at(6, true, false)
        ],
        [None, Some(Low), Some(Medium), Some(High), Some(Critical)]
    );
    assert_eq!(at(6, false, false), Some(Low), "a flat chain stays low");
    assert_eq!(
        at(4, false, true),
        Some(Medium),
        "a negation makes it a knot"
    );
    let mut f = function("f");
    f.conditions = vec![condition(3, true, false), condition(5, true, true)];
    let found = run(
        "complex_conditional",
        &file(vec![f], vec![], 100),
        None,
        None,
    );
    assert_eq!(found.len(), 2, "one finding per condition");
    assert_eq!(found[0].start_line, 12);
}

#[test]
fn complex_method_steps() {
    let at = |ccn| severity_of("complex_method", |f| f.ccn = ccn);
    assert_eq!(
        [at(8), at(9), at(14), at(15), at(24), at(25)],
        [
            None,
            Some(Medium),
            Some(Medium),
            Some(High),
            Some(High),
            Some(Critical)
        ]
    );
}

#[test]
fn large_method_steps_and_straight_line_code() {
    let at = |nloc, ccn| {
        severity_of("large_method", |f| {
            f.nloc = nloc;
            f.ccn = ccn;
        })
    };
    assert_eq!(
        [
            at(59, 5),
            at(60, 2),
            at(60, 3),
            at(90, 3),
            at(120, 3),
            at(200, 3)
        ],
        [
            None,
            None,
            Some(Low),
            Some(Medium),
            Some(High),
            Some(Critical)
        ]
    );
}

#[test]
fn bumpy_road_steps_and_simple_functions() {
    let at = |bumps, ccn| {
        severity_of("bumpy_road", |f| {
            f.bumps = bumps;
            f.ccn = ccn;
        })
    };
    assert_eq!(
        [at(2, 9), at(3, 4), at(3, 5), at(4, 5), at(5, 5)],
        [None, None, Some(Low), Some(Medium), Some(High)]
    );
}

#[test]
fn brain_method_steps_and_the_centrality_floor() {
    let big = |nloc, ccn| {
        let mut f = function("brain");
        f.nloc = nloc;
        f.ccn = ccn;
        file(vec![f], vec![], 500)
    };
    let sev = |metrics: &_, importers, p80| {
        run("brain_method", metrics, importers, p80)
            .first()
            .map(|f| f.severity)
    };
    let m = big(70, 9);
    assert_eq!(
        sev(&m, None, None),
        None,
        "silent when importers are unknown"
    );
    assert_eq!(
        sev(&m, Some(7), None),
        None,
        "the floor is 8 without a repository percentile"
    );
    assert_eq!(sev(&m, Some(8), None), Some(Medium));
    assert_eq!(
        sev(&m, Some(3), Some(1)),
        Some(Medium),
        "a sparse repository lowers the floor to 3"
    );
    assert_eq!(sev(&m, Some(2), Some(1)), None);
    assert_eq!(
        sev(&m, Some(7), Some(20)),
        None,
        "the floor never exceeds 8"
    );
    assert_eq!(sev(&big(69, 9), Some(10), None), None);
    assert_eq!(sev(&big(70, 8), Some(10), None), None);
    assert_eq!(sev(&big(70, 14), Some(10), None), Some(High));
    assert_eq!(sev(&big(120, 9), Some(10), None), Some(High));
    assert_eq!(sev(&big(149, 20), Some(10), None), Some(High));
    assert_eq!(sev(&big(150, 20), Some(10), None), Some(Critical));
}

fn methods(
    count: usize,
    brain: Option<(u32, u32)>,
) -> Vec<harness_code_analysis::metrics::FunctionMetrics> {
    (0..count)
        .map(|i| {
            let mut f = function(&format!("m{i}"));
            if i == 0 {
                if let Some((nloc, ccn)) = brain {
                    f.nloc = nloc;
                    f.ccn = ccn;
                }
            }
            f
        })
        .collect()
}

#[test]
fn god_class_needs_size_methods_and_a_brain() {
    let sev = |nloc, count: usize, brain| {
        let fs = methods(count, brain);
        let c = class("Big", nloc, (0..count).collect(), 1);
        run("god_class", &file(fs, vec![c], 500), None, None)
            .first()
            .map(|f| f.severity)
    };
    let brain = Some((70, 9));
    assert_eq!(sev(199, 15, brain), None);
    assert_eq!(sev(200, 14, brain), None);
    assert_eq!(sev(200, 15, None), None, "no brain method, no god class");
    assert_eq!(sev(200, 15, Some((69, 9))), None);
    assert_eq!(sev(200, 15, brain), Some(Medium));
    assert_eq!(sev(300, 15, brain), Some(High));
    assert_eq!(sev(200, 20, brain), Some(High));
    assert_eq!(sev(400, 24, brain), Some(High));
    assert_eq!(sev(400, 25, brain), Some(Critical));
    let found = run(
        "god_class",
        &file(
            methods(15, brain),
            vec![class("Big", 200, (0..15).collect(), 1)],
            500,
        ),
        None,
        None,
    );
    assert_eq!(found[0].class.as_deref(), Some("Big"));
    assert!(found[0].reason.contains("`m0`"), "{}", found[0].reason);
}

#[test]
fn low_cohesion_steps() {
    let sev = |lcom4, count: usize| {
        run(
            "low_cohesion",
            &file(
                methods(count, None),
                vec![class("C", 100, (0..count).collect(), lcom4)],
                500,
            ),
            None,
            None,
        )
        .first()
        .map(|f| f.severity)
    };
    assert_eq!(
        [
            sev(1, 10),
            sev(2, 4),
            sev(2, 5),
            sev(3, 5),
            sev(2, 20),
            sev(4, 14),
            sev(4, 15)
        ],
        [
            None,
            None,
            Some(Medium),
            Some(High),
            Some(High),
            Some(High),
            Some(Critical)
        ]
    );
}

#[test]
fn primitive_obsession_rules() {
    let at = |change: &dyn Fn(&mut harness_code_analysis::metrics::FunctionMetrics)| {
        severity_of("primitive_obsession", change)
    };
    assert_eq!(at(&|f| f.params = 4), None);
    assert_eq!(
        at(&|f| f.params = 5),
        Some(Low),
        "untyped: judged on the count"
    );
    assert_eq!(at(&|f| f.params = 7), Some(Medium));
    assert_eq!(at(&|f| f.params = 9), Some(High));
    assert_eq!(
        at(&|f| {
            f.params = 6;
            f.constructor = true;
        }),
        None,
        "constructors get two more"
    );
    assert_eq!(
        at(&|f| {
            f.params = 7;
            f.constructor = true;
        }),
        Some(Low)
    );
    assert_eq!(
        at(&|f| {
            f.params = 9;
            f.inherited_signature = true;
        }),
        None,
        "a trait or base class decides the signature"
    );
    assert_eq!(
        at(&|f| {
            f.params = 5;
            f.typed_params = 5;
            f.primitive_params = 2;
        }),
        None,
        "mostly rich types already"
    );
    assert_eq!(
        at(&|f| {
            f.params = 5;
            f.typed_params = 5;
            f.primitive_params = 3;
        }),
        Some(Low)
    );
    let mut f = function("f");
    f.params = 9;
    assert!(
        run(
            "primitive_obsession",
            &file(vec![f], vec![], 59),
            None,
            None
        )
        .is_empty(),
        "small files are left alone"
    );
}

#[test]
fn findings_carry_identity_numbers_and_reasons() {
    let mut f = function("Store.load");
    f.ccn = 16;
    let found = run("complex_method", &file(vec![f], vec![], 100), None, None);
    let finding = &found[0];
    assert_eq!(finding.id, "complex_method:src/a.rs:10:Store.load");
    assert_eq!(
        (finding.category, finding.detector_version),
        (Category::SizeAndComplexity, 1)
    );
    assert_eq!(finding.measured.get("ccn"), Some(&16));
    assert_eq!(finding.measured.get("limit"), Some(&9));
    assert!(finding
        .reason
        .starts_with("`Store.load` has cyclomatic complexity 16"));
}

#[test]
fn the_registry_runs_in_order_and_respects_disabled_detectors() {
    let ids: Vec<&str> = registry().iter().map(|d| d.id()).collect();
    assert_eq!(
        ids,
        [
            "nested_complexity",
            "complex_conditional",
            "god_class",
            "brain_method",
            "low_cohesion",
            "bumpy_road",
            "complex_method",
            "large_method",
            "primitive_obsession",
            "change_entropy",
            "ownership_risk",
            "churn_risk",
            "prior_defect",
            "developer_congestion",
            "knowledge_loss",
            "co_change_scatter"
        ]
    );
    assert!(registry()
        .iter()
        .all(|d| !d.summary().is_empty() && d.version() == 1));
    let mut f = function("f");
    f.ccn = 30;
    f.max_nesting = 7;
    let metrics = file(vec![f], vec![], 100);
    let context = harness_code_health::FileContext::new("a.rs", &metrics);
    let mut config = harness_code_health::DetectorConfig::default();
    assert_eq!(harness_code_health::detect_file(&context, &config).len(), 2);
    config.disabled.insert("complex_method".into());
    let left = harness_code_health::detect_file(&context, &config);
    assert_eq!(
        left.iter().map(|f| f.detector.as_str()).collect::<Vec<_>>(),
        ["nested_complexity"]
    );
    assert_ne!(
        config.digest(),
        harness_code_health::DetectorConfig::default().digest()
    );
}
