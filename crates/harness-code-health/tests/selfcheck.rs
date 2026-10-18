//! The self-check: are the lowest scores on the files that keep getting fixed?
use std::collections::BTreeMap;

use harness_code_health::{self_check, DimensionScore, FileHealth, FileScore, SelfCheckConfig};

fn health(path: &str, score: Option<f64>, fixes: Option<u32>, test: bool) -> FileHealth {
    let dimension = |s: f64| DimensionScore {
        score: s,
        unclamped: s,
        categories: BTreeMap::new(),
    };
    FileHealth {
        path: path.into(),
        nloc: 100,
        test,
        hotspot: false,
        score: score.map(|s| FileScore {
            defect: dimension(s),
            maintainability: dimension(s),
            structure_deduction: 10.0 - s,
            history_deduction: 0.0,
            impacts: BTreeMap::new(),
        }),
        fix_commits: fixes,
    }
}

/// `n` files scored 1.0, 1.3, 1.6 … (worst first by name); the files at the given ranks were fixed.
fn repository(n: usize, fixed: &[usize]) -> Vec<FileHealth> {
    (0..n)
        .map(|i| {
            let fixes = if fixed.contains(&i) { 2 } else { 0 };
            health(
                &format!("f{i:02}.rs"),
                Some(1.0 + i as f64 * 0.3),
                Some(fixes),
                false,
            )
        })
        .collect()
}

#[test]
fn precision_against_the_base_rate() {
    // 30 files, 6 fixed; five of them among the 20 worst, one at rank 25.
    let files = repository(30, &[0, 3, 7, 12, 19, 25]);
    let check = self_check(&files, 180, &SelfCheckConfig::default()).unwrap();
    assert_eq!(
        (
            check.examined,
            check.fixed_among_worst,
            check.files,
            check.fixed_files
        ),
        (20, 5, 30, 6)
    );
    assert_eq!(
        (
            check.precision_per_mille,
            check.base_rate_per_mille,
            check.recall_per_mille
        ),
        (250, 200, 833)
    );
    assert_eq!(check.lift, 1.25);
    assert_eq!(check.worst[0], ("f00.rs".to_owned(), true));
    assert_eq!(check.worst.len(), 20);
    assert_eq!(check.window_days, 180);
    assert!(check.note.contains("not a prediction"));
}

#[test]
fn hidden_when_there_is_too_little_to_say() {
    assert!(
        self_check(
            &repository(24, &[0, 1, 2, 3, 4]),
            180,
            &SelfCheckConfig::default()
        )
        .is_none(),
        "fewer than 25 files"
    );
    assert!(
        self_check(
            &repository(40, &[0, 1, 2, 3]),
            180,
            &SelfCheckConfig::default()
        )
        .is_none(),
        "fewer than 5 fixed files"
    );
    assert!(self_check(
        &repository(25, &[0, 1, 2, 3, 4]),
        180,
        &SelfCheckConfig::default()
    )
    .is_some());
}

#[test]
fn only_scored_production_files_with_history_count() {
    let mut files = repository(25, &[0, 1, 2, 3, 4]);
    // Neither a worse test, an unscored file, nor a file without history enters the ranking.
    files.push(health("tests/worse.rs", Some(0.5), Some(3), true));
    files.push(health("legacy.go", None, Some(9), false));
    files.push(health("new.rs", Some(0.5), None, false));
    let check = self_check(&files, 180, &SelfCheckConfig::default()).unwrap();
    assert_eq!((check.files, check.fixed_files), (25, 5));
    assert_eq!(check.worst[0].0, "f00.rs");
}

#[test]
fn fewer_files_than_k_examines_them_all() {
    let config = SelfCheckConfig {
        k: 20,
        min_files: 10,
        min_fixed: 2,
    };
    let check = self_check(&repository(12, &[0, 11]), 90, &config).unwrap();
    assert_eq!(
        (check.examined, check.fixed_among_worst, check.lift),
        (12, 2, 1.0)
    );
}

#[test]
fn ties_on_the_floor_break_by_depth() {
    let config = SelfCheckConfig {
        k: 1,
        min_files: 2,
        min_fixed: 1,
    };
    let mut shallow = health("a.rs", Some(1.0), Some(0), false);
    shallow.score.as_mut().unwrap().structure_deduction = 9.5;
    let mut deep = health("b.rs", Some(1.0), Some(1), false);
    deep.score.as_mut().unwrap().structure_deduction = 14.0;
    let check = self_check(&[shallow, deep], 180, &config).unwrap();
    assert_eq!(check.worst, [("b.rs".to_owned(), true)]);
}
