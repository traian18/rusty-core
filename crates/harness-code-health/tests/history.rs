//! History detectors at every severity edge, on hand-built Git signals.
mod support;

use harness_code_health::{detect_file, DetectorConfig, FileContext, Severity, Severity::*};
use harness_git_analysis::{Contributor, ContributorId, FileSignals, SignalSummary};
use support::*;

fn person(name: &str) -> Contributor {
    Contributor {
        id: ContributorId(name.to_lowercase()),
        name: name.into(),
    }
}

/// A file with history but nothing remarkable in it.
fn quiet() -> FileSignals {
    FileSignals {
        file: harness_workspace::repository_intelligence::RootRelativePath::new("src/a.rs")
            .unwrap(),
        commits: 10,
        commits_recent: 5,
        lines_changed_recent: 0,
        fix_commits: 0,
        fix_examples: Vec::new(),
        decayed_churn_milli: 0,
        churn_percentile_per_mille: 0,
        hotspot: false,
        change_entropy_milli: 0,
        change_entropy_percentile_per_mille: 0,
        contributors: 1,
        owner: Some(person("Alice")),
        owner_share_per_mille: 1000,
        recent_owner: Some(person("Alice")),
        recent_owner_share_per_mille: 1000,
        minor_contributors: 0,
        bus_factor: 1,
        age_days: 100,
        idle_days: 1,
        fix_percentile_per_mille: 0,
        recent_contributors: 1,
        owner_recent_share_per_mille: 1000,
        co_change_partners: 0,
        co_change_percentile_per_mille: 0,
    }
}

fn summary(active: u32) -> SignalSummary {
    SignalSummary {
        version: 1,
        fix_rule_version: 1,
        as_of: Some(0),
        recent_days: 90,
        fix_window_days: 180,
        hotspots: 0,
        active_contributors_recent: active,
        fix_commits: 0,
    }
}

struct Case {
    nloc: u32,
    path: &'static str,
    test: bool,
    active: u32,
}

const DEFAULT: Case = Case {
    nloc: 100,
    path: "src/a.rs",
    test: false,
    active: 10,
};

fn severity_with(
    detector: &str,
    case: Case,
    change: impl Fn(&mut FileSignals),
) -> Option<Severity> {
    let mut signals = quiet();
    change(&mut signals);
    let metrics = file(vec![], vec![], case.nloc);
    let summary = summary(case.active);
    let context = FileContext::new(case.path, &metrics)
        .with_history(&signals, &summary)
        .with_test(case.test);
    let found: Vec<_> = detect_file(&context, &DetectorConfig::default())
        .into_iter()
        .filter(|f| f.detector == detector)
        .collect();
    assert!(found.len() <= 1);
    found.first().map(|f| {
        assert_eq!(
            (f.start_line, f.function.as_deref()),
            (1, None),
            "history findings are about the file"
        );
        assert!(!f.reason.is_empty());
        f.severity
    })
}

fn severity(detector: &str, change: impl Fn(&mut FileSignals)) -> Option<Severity> {
    severity_with(detector, DEFAULT, change)
}

#[test]
fn nothing_fires_on_a_quiet_file_or_without_history() {
    let metrics = file(vec![], vec![], 100);
    assert!(detect_file(
        &FileContext::new("a.rs", &metrics),
        &DetectorConfig::default()
    )
    .is_empty());
    let signals = quiet();
    let summary = summary(10);
    assert!(detect_file(
        &FileContext::new("a.rs", &metrics).with_history(&signals, &summary),
        &DetectorConfig::default()
    )
    .is_empty());
}

#[test]
fn change_entropy_steps() {
    let at = |pct, recent, hotspot| {
        severity("change_entropy", |s| {
            s.change_entropy_milli = 500;
            s.change_entropy_percentile_per_mille = pct;
            s.commits_recent = recent;
            s.hotspot = hotspot;
        })
    };
    assert_eq!(
        [
            at(799, 3, false),
            at(800, 3, false),
            at(900, 3, false),
            at(950, 3, false),
            at(950, 3, true),
            at(900, 2, false)
        ],
        [
            None,
            Some(Medium),
            Some(High),
            Some(High),
            Some(Critical),
            None
        ]
    );
    assert_eq!(
        severity("change_entropy", |s| s
            .change_entropy_percentile_per_mille =
            990),
        None,
        "no entropy, no finding"
    );
}

#[test]
fn ownership_risk_steps() {
    let at = |commits, minor, owner, hotspot| {
        severity("ownership_risk", |s| {
            s.commits = commits;
            s.minor_contributors = minor;
            s.owner_share_per_mille = owner;
            s.hotspot = hotspot;
        })
    };
    assert_eq!(at(4, 6, 100, true), None, "too few commits to say");
    assert_eq!(at(10, 2, 400, false), None);
    assert_eq!(at(10, 2, 399, false), Some(Low), "no main author");
    assert_eq!(at(10, 3, 800, false), Some(Medium));
    assert_eq!(at(10, 3, 800, true), Some(High));
    assert_eq!(at(10, 5, 800, false), Some(High));
    assert_eq!(at(10, 6, 800, false), Some(High));
    assert_eq!(at(10, 6, 800, true), Some(Critical));
    let small = severity_with(
        "ownership_risk",
        Case {
            nloc: 39,
            ..DEFAULT
        },
        |s| {
            s.minor_contributors = 6;
            s.hotspot = true;
        },
    );
    assert_eq!(small, Some(Low), "small files stay low");
}

#[test]
fn churn_risk_steps() {
    // The file has 100 lines of code, so lines changed / 100 is the relative churn.
    let at = |lines, recent, pct, hotspot| {
        severity("churn_risk", |s| {
            s.lines_changed_recent = lines;
            s.commits_recent = recent;
            s.churn_percentile_per_mille = pct;
            s.hotspot = hotspot;
        })
    };
    assert_eq!(
        [
            at(99, 5, 750, false),
            at(100, 5, 750, false),
            at(150, 5, 750, false),
            at(250, 5, 750, false),
            at(400, 5, 750, false),
            at(400, 5, 750, true)
        ],
        [
            None,
            Some(Low),
            Some(Medium),
            Some(High),
            Some(High),
            Some(Critical)
        ]
    );
    assert_eq!(at(400, 4, 750, true), None);
    assert_eq!(at(400, 5, 749, true), None);
    assert_eq!(
        severity_with("churn_risk", Case { nloc: 0, ..DEFAULT }, |s| {
            s.lines_changed_recent = 400;
            s.churn_percentile_per_mille = 900;
        }),
        None
    );
}

#[test]
fn prior_defect_steps() {
    let at = |fixes, pct, hotspot| {
        severity("prior_defect", |s| {
            s.fix_commits = fixes;
            s.fix_percentile_per_mille = pct;
            s.hotspot = hotspot;
        })
    };
    assert_eq!(
        [
            at(0, 900, false),
            at(1, 799, false),
            at(1, 800, false),
            at(2, 800, false),
            at(3, 800, false),
            at(3, 800, true),
            at(5, 800, false)
        ],
        [
            None,
            None,
            Some(Low),
            Some(Medium),
            Some(High),
            Some(Critical),
            Some(Critical)
        ]
    );
}

#[test]
fn developer_congestion_steps() {
    let at = |people, recent, owner, pct| {
        severity("developer_congestion", |s| {
            s.recent_contributors = people;
            s.commits_recent = recent;
            s.owner_share_per_mille = owner;
            s.churn_percentile_per_mille = pct;
        })
    };
    assert_eq!(at(5, 3, 499, 750), Some(Low));
    assert_eq!(at(5, 3, 500, 750), None, "someone leads it");
    assert_eq!(at(4, 3, 100, 750), None);
    assert_eq!(at(5, 2, 100, 750), None);
    assert_eq!(at(5, 3, 100, 749), None);
    assert_eq!(at(7, 3, 100, 750), Some(Medium));
    assert_eq!(at(5, 12, 100, 750), Some(Medium));
    assert_eq!(at(10, 19, 100, 750), Some(Medium));
    assert_eq!(at(10, 20, 100, 750), Some(High));
}

#[test]
fn knowledge_loss_steps_and_small_teams() {
    let replaced = |s: &mut FileSignals| {
        s.recent_owner = Some(person("Bob"));
        s.owner_recent_share_per_mille = 0;
    };
    assert_eq!(
        severity("knowledge_loss", replaced),
        Some(Medium),
        "replaced and quiet"
    );
    assert_eq!(
        severity("knowledge_loss", |s| {
            replaced(s);
            s.hotspot = true;
        }),
        Some(High)
    );
    assert_eq!(
        severity("knowledge_loss", |s| {
            replaced(s);
            s.owner_recent_share_per_mille = 300;
        }),
        Some(Low),
        "replaced but still around"
    );
    assert_eq!(
        severity("knowledge_loss", |s| s.owner_recent_share_per_mille = 199),
        Some(Low),
        "quiet but still the recent owner"
    );
    assert_eq!(
        severity("knowledge_loss", |s| s.owner_recent_share_per_mille = 200),
        None
    );
    assert_eq!(
        severity("knowledge_loss", |s| {
            replaced(s);
            s.bus_factor = 2;
        }),
        None
    );
    assert_eq!(
        severity("knowledge_loss", |s| {
            replaced(s);
            s.commits_recent = 0;
        }),
        None,
        "nobody is touching it"
    );
    assert_eq!(
        severity_with(
            "knowledge_loss",
            Case {
                active: 3,
                ..DEFAULT
            },
            replaced
        ),
        Some(Low),
        "one owner per file is normal for three people"
    );
    assert_eq!(
        severity_with(
            "knowledge_loss",
            Case {
                active: 3,
                ..DEFAULT
            },
            |s| {
                replaced(s);
                s.hotspot = true;
            }
        ),
        Some(High)
    );
}

#[test]
fn co_change_scatter_steps_and_exemptions() {
    let scattered = |pct| {
        move |s: &mut FileSignals| {
            s.co_change_partners = 9;
            s.co_change_percentile_per_mille = pct;
            s.commits_recent = 3;
        }
    };
    assert_eq!(
        [
            severity("co_change_scatter", scattered(799)),
            severity("co_change_scatter", scattered(800)),
            severity("co_change_scatter", scattered(950))
        ],
        [None, Some(Medium), Some(High)]
    );
    assert_eq!(
        severity("co_change_scatter", |s| {
            scattered(950)(s);
            s.commits_recent = 2;
        }),
        None
    );
    assert_eq!(
        severity_with(
            "co_change_scatter",
            Case {
                test: true,
                ..DEFAULT
            },
            scattered(950)
        ),
        None,
        "tests change with their subject"
    );
    for barrel in ["src/mod.rs", "web/index.ts", "pkg/__init__.py"] {
        assert_eq!(
            severity_with(
                "co_change_scatter",
                Case {
                    path: barrel,
                    ..DEFAULT
                },
                scattered(950)
            ),
            None,
            "{barrel}"
        );
    }
}
