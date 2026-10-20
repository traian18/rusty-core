//! Error handling, duplication and test detectors on hand-built measurements.
mod support;

use harness_code_analysis::metrics::{ErrorStats, FunctionMetrics};
use harness_code_health::{
    detect_file, duplication::DuplicateIndex, DetectorConfig, FileContext, Finding, Severity,
    Severity::*, TestLinks,
};
use support::*;

fn only(detector: &str, findings: Vec<Finding>) -> Vec<Finding> {
    findings
        .into_iter()
        .filter(|f| f.detector == detector)
        .collect()
}

#[test]
fn swallowed_errors_step_with_their_count() {
    let at = |empty, broad, ignored| {
        severity_of("swallowed_error", |f| {
            f.errors = ErrorStats {
                empty_handlers: empty,
                broad_handlers: broad,
                panics: 0,
                ignored_results: ignored,
            }
        })
    };
    assert_eq!(
        [
            at(0, 0, 0),
            at(1, 0, 0),
            at(0, 0, 1), // one discarded result is not enough on its own
            at(0, 0, 2),
            at(1, 1, 0),
            at(2, 1, 2),
            at(4, 0, 0)
        ],
        [
            None,
            Some(Low),
            None,
            Some(Low),
            Some(Medium),
            Some(High),
            Some(High)
        ]
    );
}

#[test]
fn panics_need_a_threshold_and_tests_are_exempt() {
    let at = |n| severity_of("panic_prone", |f| f.errors.panics = n);
    assert_eq!(
        [at(4), at(5), at(10), at(20)],
        [None, Some(Low), Some(Medium), Some(High)]
    );
    let mut test = function("t");
    test.test = true;
    test.test_code = true;
    test.errors.panics = 30;
    let metrics = file(vec![test], Vec::new(), 100);
    assert!(run("panic_prone", &metrics, None, None).is_empty());
    // And a swallowed error inside a test file is not reported either.
    let mut f = function("f");
    f.errors.empty_handlers = 3;
    let metrics = file(vec![f], Vec::new(), 100);
    let context = FileContext::new("tests/a.rs", &metrics).with_test(true);
    assert!(only(
        "swallowed_error",
        detect_file(&context, &DetectorConfig::default())
    )
    .is_empty());
}

fn clone_of(name: &str, shape: u64, nloc: u32) -> FunctionMetrics {
    let mut f = function(name);
    f.shape = shape;
    f.shape_nodes = 80;
    f.nloc = nloc;
    f
}

#[test]
fn near_copies_are_found_across_files_and_reported_with_their_places() {
    let a = file(
        vec![clone_of("build", 7, 20), clone_of("other", 9, 20)],
        Vec::new(),
        100,
    );
    let b = file(vec![clone_of("make", 7, 20)], Vec::new(), 100);
    let c = file(vec![clone_of("make", 7, 20)], Vec::new(), 100);
    let tests = file(vec![clone_of("setup", 7, 20)], Vec::new(), 100);
    let config = DetectorConfig::default();
    let index = DuplicateIndex::build(
        [
            ("src/a.rs", &a, false),
            ("src/b.rs", &b, false),
            ("src/c.rs", &c, false),
            ("tests/t.rs", &tests, true),
        ],
        &config.duplicated_code,
    );
    let found = index.duplicates_in("src/a.rs");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].function, "build");
    // Two other places, and the test file does not count.
    assert_eq!(found[0].copies.len(), 2);
    assert!(index.duplicates_in("tests/t.rs").is_empty());
    let context = FileContext::new("src/a.rs", &a).with_duplicates(index.duplicates_in("src/a.rs"));
    let findings = only("duplicated_code", detect_file(&context, &config));
    // 20 lines is medium; three places raises it one step.
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, High);
    assert!(
        findings[0].reason.contains("src/b.rs:10"),
        "{}",
        findings[0].reason
    );
}

#[test]
fn small_or_unique_functions_are_not_duplicates() {
    let mut tiny = clone_of("t", 5, 3);
    tiny.shape_nodes = 10;
    let a = file(vec![tiny.clone(), clone_of("u", 6, 20)], Vec::new(), 50);
    let b = file(vec![tiny, clone_of("v", 8, 20)], Vec::new(), 50);
    let index = DuplicateIndex::build(
        [("a.rs", &a, false), ("b.rs", &b, false)],
        &DetectorConfig::default().duplicated_code,
    );
    assert!(index.is_empty());
    // Same shape inside one file counts as a duplicate too.
    let same = file(
        vec![clone_of("x", 1, 12), clone_of("y", 1, 12)],
        Vec::new(),
        50,
    );
    let index = DuplicateIndex::build(
        [("s.rs", &same, false)],
        &DetectorConfig::default().duplicated_code,
    );
    assert_eq!(index.duplicates_in("s.rs").len(), 2);
}

fn untested(links: TestLinks, importers: Option<u32>, hotspot: bool) -> Vec<Severity> {
    let mut f = function("f");
    f.ccn = 5;
    let metrics = file(vec![f], Vec::new(), 120);
    let mut context = FileContext::new("src/a.rs", &metrics).with_tests(&links);
    if let Some(i) = importers {
        context = context.with_importers(i, Some(10));
    }
    let _ = hotspot;
    only(
        "untested_code",
        detect_file(&context, &DetectorConfig::default()),
    )
    .into_iter()
    .map(|f| f.severity)
    .collect()
}

#[test]
fn untested_code_needs_tests_to_exist_and_no_link() {
    let none = TestLinks {
        repo_tests: 10,
        ..TestLinks::default()
    };
    assert_eq!(untested(none, Some(1), false), [Low]);
    assert_eq!(untested(none, Some(40), false), [Medium]);
    // Linked by import, by name, indirectly or by tests in the file.
    for linked in [
        TestLinks {
            test_files: 1,
            ..none
        },
        TestLinks {
            indirect: 1,
            ..none
        },
        TestLinks {
            sibling: true,
            ..none
        },
        TestLinks {
            in_file: true,
            ..none
        },
    ] {
        assert!(untested(linked, Some(40), false).is_empty());
    }
    // No tests in the repository is no signal.
    assert!(untested(
        TestLinks {
            repo_tests: 1,
            ..TestLinks::default()
        },
        Some(40),
        false
    )
    .is_empty());
}

#[test]
fn tests_without_assertions_and_skipped_tests() {
    let mut empty = function("t_empty");
    empty.test = true;
    empty.nloc = 12;
    let mut checked = function("t_ok");
    checked.test = true;
    checked.assertions = 2;
    let mut smoke = function("t_smoke");
    smoke.test = true;
    smoke.nloc = 2;
    let mut metrics = file(vec![empty, checked, smoke], Vec::new(), 100);
    metrics.skipped_tests = 3;
    let context = FileContext::new("tests/a.rs", &metrics);
    let findings = detect_file(&context, &DetectorConfig::default());
    let no_assert = only("test_without_assertions", findings.clone());
    assert_eq!(
        no_assert
            .iter()
            .map(|f| (f.function.clone().unwrap(), f.severity))
            .collect::<Vec<_>>(),
        [("t_empty".to_owned(), Medium)]
    );
    assert_eq!(only("skipped_tests", findings)[0].severity, Low);
}

#[test]
fn an_edited_file_is_compared_against_the_rest_without_its_own_stale_copies() {
    let a = file(vec![clone_of("build", 7, 20)], Vec::new(), 100);
    let b = file(vec![clone_of("make", 7, 20)], Vec::new(), 100);
    let config = DetectorConfig::default();
    let index = DuplicateIndex::build(
        [("a.rs", &a, false), ("b.rs", &b, false)],
        &config.duplicated_code,
    );
    // Edit a.rs so build is still a copy, then so it is unique.
    let live = index.duplicates_for("a.rs", &a, &config.duplicated_code);
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].copies[0].path, "b.rs");
    let changed = file(vec![clone_of("build", 99, 20)], Vec::new(), 100);
    assert!(index
        .duplicates_for("a.rs", &changed, &config.duplicated_code)
        .is_empty());
}
