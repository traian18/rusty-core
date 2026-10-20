use std::collections::BTreeMap;

use harness_code_health::{
    rollup::FileHealth,
    scoring::{score_file, ScoringConfig},
    trend::{direction, file_scores, moves, PointMeta, TrendPoint, TrendSeries},
    Category, Finding, Severity,
};

fn finding(detector: &str, severity: Severity) -> Finding {
    Finding {
        id: format!("{detector}:a:1:"),
        detector: detector.into(),
        detector_version: 1,
        category: Category::StructuralComplexity,
        severity,
        path: "a".into(),
        function: None,
        class: None,
        start_line: 1,
        end_line: 1,
        measured: BTreeMap::new(),
        reason: String::new(),
    }
}

fn health(path: &str, findings: &[Finding]) -> FileHealth {
    FileHealth {
        path: path.into(),
        nloc: 100,
        test: false,
        hotspot: false,
        score: Some(score_file(findings, &ScoringConfig::default())),
        fix_commits: None,
    }
}

fn meta(at: u64, commit: &str, backfilled: bool) -> PointMeta {
    PointMeta {
        at_ms: at,
        commit: Some(commit.into()),
        backfilled,
        digest: "d".into(),
    }
}

#[test]
fn a_point_summarises_scores_and_findings() {
    let bad = [
        finding("nested_complexity", Severity::High),
        finding("large_method", Severity::Low),
    ];
    let files = [health("a.rs", &bad), health("b.rs", &[])];
    let all: Vec<&Finding> = bad.iter().collect();
    let point = TrendPoint::from_files(&files, all, meta(10, "c1", false));
    assert_eq!(
        (point.findings, point.severities.high, point.severities.low),
        (2, 1, 1)
    );
    assert_eq!(point.scored_files, 2);
    assert!(point.structure_score.unwrap() < 10.0 && point.structure_score.unwrap() > 5.0);
    assert_eq!(point.top_detectors.len(), 2);
}

#[test]
fn movers_are_the_files_whose_structure_score_changed() {
    let bad = [finding("nested_complexity", Severity::Critical)];
    let before = file_scores(&[
        health("a.rs", &[]),
        health("b.rs", &bad),
        health("c.rs", &[]),
    ]);
    let after = file_scores(&[
        health("a.rs", &bad),
        health("b.rs", &[]),
        health("c.rs", &[]),
        health("new.rs", &bad),
    ]);
    let (down, up) = moves(&before, &after);
    assert_eq!(
        down.iter().map(|m| m.path.as_str()).collect::<Vec<_>>(),
        ["a.rs"]
    );
    assert_eq!(
        up.iter().map(|m| m.path.as_str()).collect::<Vec<_>>(),
        ["b.rs"]
    );
    assert!(down[0].to < down[0].from);
}

#[test]
fn the_series_dedupes_orders_and_names_regressions() {
    let clean = [health("a.rs", &[])];
    let bad = [health(
        "a.rs",
        &[finding("nested_complexity", Severity::Critical)],
    )];
    let mut series = TrendSeries::default();
    let point = |files: &[FileHealth], at, commit, back| {
        TrendPoint::from_files(files, [], meta(at, commit, back))
    };
    series.push(point(&clean, 100, "c1", false), Some(&file_scores(&clean)));
    series.push(point(&bad, 200, "c2", false), Some(&file_scores(&bad)));
    // The same commit again replaces the point rather than adding one.
    series.push(point(&bad, 300, "c2", false), Some(&file_scores(&bad)));
    assert_eq!(series.points.len(), 2);
    assert_eq!(series.latest().unwrap().at_ms, 300);
    let first_regression = &series.points[1].regressions;
    // The replaced point compared against the stored scores, which were already "bad".
    assert!(first_regression.is_empty());
    // A backfilled point sorts by its own time and carries no movers.
    series.push(point(&clean, 50, "old", true), None);
    assert_eq!(series.points[0].commit.as_deref(), Some("old"));
    assert!(series.points[0].regressions.is_empty());
    assert_eq!(series.commits(true).len(), 1);
    assert_eq!(direction(&series.points, 0.1), "declining");
    assert_eq!(series.before(100).unwrap().at_ms, 100);
}

#[test]
fn the_first_recorded_point_after_a_regression_names_the_file() {
    let clean = [health("a.rs", &[])];
    let bad = [health(
        "a.rs",
        &[finding("nested_complexity", Severity::Critical)],
    )];
    let mut series = TrendSeries::default();
    let at = |files: &[FileHealth], t, c| TrendPoint::from_files(files, [], meta(t, c, false));
    series.push(at(&clean, 1, "c1"), Some(&file_scores(&clean)));
    series.push(at(&bad, 2, "c2"), Some(&file_scores(&bad)));
    assert_eq!(series.latest().unwrap().regressions[0].path, "a.rs");
}
