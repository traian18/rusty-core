//! Repository and folder roll-ups over hand-built file scores, with every average worked out.
use std::collections::BTreeMap;

use harness_code_health::{
    folder_rollups, repository_health, Band, DimensionScore, FileHealth, FileScore, RollupConfig,
};

fn dimension(score: f64) -> DimensionScore {
    DimensionScore {
        score,
        unclamped: score,
        categories: BTreeMap::new(),
    }
}

struct F {
    path: &'static str,
    nloc: u32,
    defect: Option<f64>,
    maintainability: f64,
    structure: f64,
    history: f64,
    test: bool,
    hotspot: bool,
}

const BASE: F = F {
    path: "",
    nloc: 100,
    defect: Some(10.0),
    maintainability: 10.0,
    structure: 0.0,
    history: 0.0,
    test: false,
    hotspot: false,
};

fn file(f: F) -> FileHealth {
    FileHealth {
        path: f.path.into(),
        nloc: f.nloc,
        test: f.test,
        hotspot: f.hotspot,
        fix_commits: None,
        score: f.defect.map(|d| FileScore {
            defect: dimension(d),
            maintainability: dimension(f.maintainability),
            structure_deduction: f.structure,
            history_deduction: f.history,
            impacts: BTreeMap::new(),
        }),
    }
}

fn repository() -> Vec<FileHealth> {
    vec![
        file(F {
            path: "src/a.rs",
            defect: Some(9.0),
            maintainability: 9.5,
            history: 1.0,
            hotspot: true,
            ..BASE
        }),
        file(F {
            path: "src/b.rs",
            nloc: 300,
            defect: Some(5.0),
            maintainability: 6.0,
            structure: 4.0,
            history: 1.0,
            ..BASE
        }),
        file(F {
            path: "web/c.ts",
            defect: Some(3.0),
            maintainability: 4.0,
            structure: 5.0,
            history: 2.0,
            hotspot: true,
            ..BASE
        }),
        file(F {
            path: "tests/t.rs",
            nloc: 50,
            defect: Some(6.0),
            maintainability: 7.0,
            structure: 3.0,
            history: 1.0,
            test: true,
            ..BASE
        }),
        file(F {
            path: "src/legacy.go",
            nloc: 400,
            defect: None,
            ..BASE
        }),
        // Empty, but still weighs one line.
        file(F {
            path: "build.rs",
            nloc: 0,
            ..BASE
        }),
    ]
}

#[test]
fn bands_have_absolute_edges() {
    let at = |s| Band::of(s);
    assert_eq!(
        [
            at(8.5),
            at(8.49),
            at(7.0),
            at(6.99),
            at(5.5),
            at(5.49),
            at(4.0),
            at(3.99),
            at(1.0)
        ],
        [
            Band::Excellent,
            Band::Good,
            Band::Good,
            Band::Fair,
            Band::Fair,
            Band::NeedsWork,
            Band::NeedsWork,
            Band::AtRisk,
            Band::AtRisk
        ]
    );
    assert_eq!(Band::NeedsWork.label(), "Needs work");
}

#[test]
fn line_weighted_headline_hotspots_and_dimensions() {
    let health = repository_health(&repository(), &RollupConfig::default());
    // (9×100 + 5×300 + 3×100 + 6×50 + 10×1) / 551.
    assert_eq!(health.average, Some(5.46));
    assert_eq!(health.band, Some(Band::NeedsWork));
    // Hotspots a.rs and c.ts: (9×100 + 3×100) / 200.
    assert_eq!(health.hotspot_health, Some(6.0));
    assert_eq!(health.maintainability_average, Some(6.37));
    assert_eq!(health.maintainability_hotspot, Some(6.75));
    // Without the test: 2710 / 501.
    assert_eq!(health.production_average, Some(5.41));
    assert_eq!(
        (health.structure_average, health.history_average),
        (Some(3.36), Some(1.18))
    );
    assert_eq!(
        (health.structure_hotspot, health.history_hotspot),
        (Some(2.5), Some(1.5))
    );
}

#[test]
fn worst_files_counts_and_the_distribution() {
    let health = repository_health(&repository(), &RollupConfig::default());
    assert_eq!(
        health
            .worst_file
            .as_ref()
            .map(|w| (w.path.as_str(), w.score)),
        Some(("web/c.ts", 3.0))
    );
    assert_eq!(
        health.worst_test.as_ref().map(|w| w.path.as_str()),
        Some("tests/t.rs")
    );
    assert_eq!(
        (
            health.file_count,
            health.production_file_count,
            health.hotspot_count,
            health.unanalysed_count
        ),
        (5, 4, 2, 1)
    );
    assert_eq!(health.analysed_per_mille, 833);
    let shares: Vec<(Band, usize, u32)> = health
        .distribution
        .iter()
        .map(|d| (d.band, d.files, d.nloc_per_mille))
        .collect();
    assert_eq!(
        shares,
        [
            (Band::AtRisk, 1, 181),
            (Band::NeedsWork, 1, 545),
            (Band::Fair, 1, 90),
            (Band::Good, 0, 0),
            (Band::Excellent, 2, 181)
        ]
    );
}

#[test]
fn no_flattering_defaults() {
    // Nothing scored: no average, no band, no worst file.
    let unscored = vec![file(F {
        path: "a.go",
        defect: None,
        ..BASE
    })];
    let health = repository_health(&unscored, &RollupConfig::default());
    assert_eq!(
        (
            health.average,
            health.band,
            health.hotspot_health,
            health.worst_file
        ),
        (None, None, None, None)
    );
    assert_eq!((health.unanalysed_count, health.analysed_per_mille), (1, 0));
    // No hotspots: hotspot health is unknown, not 10.
    let calm = vec![file(F {
        path: "a.rs",
        ..BASE
    })];
    assert_eq!(
        repository_health(&calm, &RollupConfig::default()).hotspot_health,
        None
    );
    // Too little scored for a band, though the average stands.
    let mostly_unscored = vec![
        file(F {
            path: "a.rs",
            defect: Some(9.0),
            ..BASE
        }),
        file(F {
            path: "b.go",
            defect: None,
            ..BASE
        }),
        file(F {
            path: "c.go",
            defect: None,
            ..BASE
        }),
    ];
    let health = repository_health(&mostly_unscored, &RollupConfig::default());
    assert_eq!((health.average, health.band), (Some(9.0), None));
    assert_eq!(
        repository_health(&[], &RollupConfig::default()).average,
        None
    );
}

#[test]
fn the_floor_does_not_hide_which_file_is_worse() {
    let files = vec![
        file(F {
            path: "a.rs",
            defect: Some(1.0),
            structure: 6.0,
            history: 3.5,
            ..BASE
        }),
        file(F {
            path: "b.rs",
            defect: Some(1.0),
            structure: 8.5,
            history: 3.5,
            ..BASE
        }),
    ];
    let health = repository_health(&files, &RollupConfig::default());
    assert_eq!(health.worst_file.unwrap().path, "b.rs");
}

#[test]
fn folders_roll_up_worst_first_with_their_worst_slice() {
    let folders = folder_rollups(&repository(), 1);
    let order: Vec<&str> = folders.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(order, ["web", "src", "tests", "."]);
    let src = &folders[1];
    // a.rs and b.rs scored, legacy.go not: (9×100 + 5×300) / 400.
    assert_eq!(
        (src.files, src.nloc, src.average, src.band),
        (3, 800, Some(6.0), Some(Band::Fair))
    );
    assert_eq!(
        (src.worst_decile, src.hotspots, src.unanalysed),
        (Some(5.0), 1, 1)
    );
    assert_eq!(
        src.worst_file.as_ref().map(|w| w.path.as_str()),
        Some("src/b.rs")
    );

    // Deeper cuts, and a folder with nothing scored sorts last.
    let files = vec![
        file(F {
            path: "src/x/a.rs",
            defect: Some(4.0),
            ..BASE
        }),
        file(F {
            path: "src/x/deep/b.rs",
            defect: Some(8.0),
            ..BASE
        }),
        file(F {
            path: "src/y.rs",
            defect: Some(7.0),
            ..BASE
        }),
        file(F {
            path: "vendor/z.go",
            defect: None,
            ..BASE
        }),
    ];
    let folders = folder_rollups(&files, 2);
    let order: Vec<(&str, Option<f64>)> = folders
        .iter()
        .map(|f| (f.path.as_str(), f.average))
        .collect();
    assert_eq!(
        order,
        [("src/x", Some(6.0)), ("src", Some(7.0)), ("vendor", None)]
    );
}

#[test]
fn the_worst_slice_is_a_tenth_of_the_files() {
    let files: Vec<FileHealth> = (0..20)
        .map(|i| {
            let path: &'static str = Box::leak(format!("m/f{i}.rs").into_boxed_str());
            file(F {
                path,
                defect: Some(1.0 + i as f64 * 0.4),
                ..BASE
            })
        })
        .collect();
    let m = &folder_rollups(&files, 1)[0];
    // The two lowest: 1.0 and 1.4.
    assert_eq!(m.worst_decile, Some(1.2));
}
