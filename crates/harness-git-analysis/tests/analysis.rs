mod support;

use std::{collections::BTreeSet, sync::Arc};

use harness_git_analysis::{
    ChurnQuery, ChurnScope, ChurnSort, CoChangeQuery, ContributorQuery, GitAnalysis,
    GitAnalysisBuilder, GitAnalysisConfig, GitAnalysisError, GitQueryError, GitSnapshotId,
    RepositoryCorrelation, WalkStop,
};
use harness_workspace::repository_intelligence::SnapshotId;
use support::*;
use tokio_util::sync::CancellationToken;

fn churn_query(
    snapshot: &GitSnapshotId,
    scope: ChurnScope,
    sort: ChurnSort,
    limit: usize,
) -> ChurnQuery {
    ChurnQuery {
        snapshot: snapshot.clone(),
        scope,
        sort,
        cursor: None,
        limit,
    }
}

#[tokio::test]
async fn churn_counts_commits_lines_contributors_and_hashes_emails() {
    let repo = TestRepo::new();
    repo.write("a.rs", "one\ntwo\n");
    repo.write("b.rs", "x\n");
    repo.commit("add", ALICE, START);
    repo.write("a.rs", "one\ntwo\nthree\n");
    repo.commit("edit a", BOB, START + DAY);
    repo.write("a.rs", "one\nthree\n");
    repo.commit(
        "edit a again",
        ("Alice Renamed", "ALICE@example.com"),
        START + 2 * DAY,
    );

    let service = service(&repo);
    let snapshot = refresh(&service).await;
    let a = snapshot
        .index
        .churn
        .iter()
        .find(|c| c.file == path("a.rs"))
        .unwrap();
    assert_eq!(a.commits, 3);
    assert_eq!((a.lines_added, a.lines_removed), (2 + 1, 1));
    assert_eq!(
        a.contributors, 2,
        "ALICE@example.com and alice@example.com are one contributor"
    );
    assert_eq!((a.first_change, a.last_change), (START, START + 2 * DAY));
    assert!(!a.absent_at_head);
    let b = snapshot
        .index
        .churn
        .iter()
        .find(|c| c.file == path("b.rs"))
        .unwrap();
    assert_eq!(b.commits, 1);

    let page = service
        .contributors(&ContributorQuery {
            snapshot: snapshot.id.clone(),
            file: path("a.rs"),
            cursor: None,
            limit: 10,
        })
        .unwrap();
    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.entries[0].commits, 2);
    assert_eq!(page.entries[0].commit_share_per_mille, 666);
    assert_eq!(
        page.entries[0].contributor.name, "Alice Renamed",
        "the newest spelling is shown"
    );
    assert!(page.definition.contains("not code ownership"));
    assert!(page.interpretation.contains("not show causation"));
    let json = serde_json::to_string(&*snapshot).unwrap();
    assert!(
        !json.contains("example.com"),
        "raw email addresses must never be stored"
    );
}

#[tokio::test]
async fn renames_are_followed_to_the_head_path_and_deleted_files_are_flagged() {
    let repo = TestRepo::new();
    let body = "fn main() {\n    println!(\"hello\");\n    println!(\"world\");\n    println!(\"again\");\n}\n";
    repo.write("old.rs", body);
    repo.write("gone.rs", "temporary\n");
    repo.commit("add", ALICE, START);
    repo.write("old.rs", &format!("{body}// edit\n"));
    repo.commit("edit old", ALICE, START + DAY);
    repo.rename("old.rs", "moved/new.rs");
    repo.remove("gone.rs");
    repo.commit("rename and delete", BOB, START + 2 * DAY);
    repo.write("moved/new.rs", &format!("{body}// edit\n// edit again\n"));
    repo.commit("edit new", ALICE, START + 3 * DAY);

    let snapshot = refresh(&service(&repo)).await;
    let files = snapshot
        .index
        .churn
        .iter()
        .map(|c| c.file.as_path().to_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        files,
        vec!["gone.rs", "moved/new.rs"],
        "old.rs history belongs to moved/new.rs"
    );
    let moved = snapshot
        .index
        .churn
        .iter()
        .find(|c| c.file == path("moved/new.rs"))
        .unwrap();
    assert_eq!(moved.commits, 4);
    assert!(!moved.absent_at_head);
    let gone = snapshot
        .index
        .churn
        .iter()
        .find(|c| c.file == path("gone.rs"))
        .unwrap();
    assert!(gone.absent_at_head);
    assert_eq!(gone.commits, 2);
}

#[tokio::test]
async fn merge_commits_contribute_nothing_and_nothing_is_double_counted() {
    let repo = TestRepo::new();
    repo.write("base.rs", "base\n");
    let base = repo.commit("base", ALICE, START);
    repo.repo
        .branch("side", &repo.repo.find_commit(base).unwrap(), false)
        .unwrap();
    repo.repo.set_head("refs/heads/side").unwrap();
    repo.repo
        .checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    repo.write("side.rs", "side\n");
    let side = repo.commit("side work", BOB, START + DAY);
    repo.repo
        .set_head("refs/heads/master")
        .unwrap_or_else(|_| repo.repo.set_head("refs/heads/main").unwrap());
    repo.repo
        .checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    repo.write("main.rs", "main\n");
    repo.commit("main work", ALICE, START + 2 * DAY);
    repo.write("side.rs", "side\n");
    repo.merge("merge side", ALICE, START + 3 * DAY, side);

    let snapshot = refresh(&service(&repo)).await;
    assert_eq!(snapshot.coverage.merges_skipped, 1);
    assert_eq!(snapshot.coverage.commits_read, 4);
    let side_churn = snapshot
        .index
        .churn
        .iter()
        .find(|c| c.file == path("side.rs"))
        .unwrap();
    assert_eq!(
        side_churn.commits, 1,
        "the merge must not count side.rs again"
    );
}

#[tokio::test]
async fn generated_and_vendored_files_are_excluded() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", "pub fn f() {}\n");
    repo.write("Cargo.lock", "# lock\n");
    repo.write("dist/bundle.js", "x\n");
    repo.write("web/app.min.js", "x\n");
    repo.write("node_modules/dep/index.js", "x\n");
    repo.commit("add", ALICE, START);

    let snapshot = refresh(&service(&repo)).await;
    let files = snapshot
        .index
        .churn
        .iter()
        .map(|c| c.file.as_path().to_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(files, vec!["src/lib.rs"]);
    assert_eq!(snapshot.coverage.excluded_changes, 4);

    // Exclusions are part of the configuration, so changing them changes the snapshot.
    let config = GitAnalysisConfig {
        exclusions: vec![],
        ..GitAnalysisConfig::default()
    };
    let all = refresh(&service_with(&repo, config)).await;
    assert_ne!(all.id, snapshot.id);
    assert!(all.index.churn.len() > 1);
}

#[tokio::test]
async fn bulk_commits_count_for_churn_but_not_co_change() {
    let repo = TestRepo::new();
    for name in ["a", "b", "c", "d", "e"] {
        repo.write(&format!("{name}.rs"), "x\n");
    }
    repo.commit("bulk", ALICE, START);
    let config = GitAnalysisConfig {
        max_files_per_commit: 3,
        min_support: 1,
        min_confidence_per_mille: 0,
        ..GitAnalysisConfig::default()
    };
    let snapshot = refresh(&service_with(&repo, config)).await;
    assert_eq!(snapshot.index.churn.len(), 5);
    assert!(snapshot.index.churn.iter().all(|c| c.commits == 1));
    assert!(snapshot.index.co_changes.is_empty());
    assert_eq!(snapshot.coverage.bulk_commits_excluded, 1);
}

#[tokio::test]
async fn co_change_respects_support_and_confidence_and_keeps_examples() {
    let repo = TestRepo::new();
    let mut time = START;
    let mut step = |repo: &TestRepo, files: &[&str]| {
        for f in files {
            let existing = std::fs::read_to_string(repo.path().join(f)).unwrap_or_default();
            repo.write(f, &format!("{existing}line\n"));
        }
        time += DAY;
        repo.commit("change", ALICE, time)
    };
    // a and b change together four times; a also changes alone once; c joins twice; d joins three times but has many solo commits.
    for _ in 0..4 {
        step(&repo, &["a.rs", "b.rs"]);
    }
    step(&repo, &["a.rs"]);
    for _ in 0..2 {
        step(&repo, &["a.rs", "c.rs"]);
    }
    for _ in 0..3 {
        step(&repo, &["a.rs", "d.rs"]);
    }
    for _ in 0..12 {
        step(&repo, &["d.rs"]);
    }

    let service = service(&repo);
    let snapshot = refresh(&service).await;
    let query = |file: &str, support, confidence| CoChangeQuery {
        snapshot: snapshot.id.clone(),
        file: path(file),
        min_support: support,
        min_confidence_per_mille: confidence,
        cursor: None,
        limit: 10,
    };
    let a = service.co_changed(&query("a.rs", None, None)).unwrap();
    let partners = a
        .entries
        .iter()
        .map(|e| {
            (
                e.partner.as_path().to_str().unwrap().to_owned(),
                e.support,
                e.confidence_per_mille,
            )
        })
        .collect::<Vec<_>>();
    // a has 10 commits: b 4/10 = 400, d 3/10 = 300; c has support 2 (< 3) and is left out.
    assert_eq!(
        partners,
        vec![("b.rs".into(), 4, 400), ("d.rs".into(), 3, 300)]
    );
    assert_eq!(a.entries[0].file_commits, 10);
    assert_eq!(
        a.entries[0].example_commits.len(),
        3,
        "examples are bounded"
    );
    assert!(a.interpretation.contains("not show causation"));

    // d changed in 15 commits, so a is only 3/15 = 200 for d and below the confidence floor.
    let d = service.co_changed(&query("d.rs", None, None)).unwrap();
    assert!(d.entries.is_empty());
    // From b's side, a is nearly always present.
    let b = service.co_changed(&query("b.rs", None, None)).unwrap();
    assert_eq!(b.entries[0].confidence_per_mille, 1000);

    // Raising thresholds narrows; lowering below the snapshot's own is rejected.
    assert_eq!(
        service
            .co_changed(&query("a.rs", Some(4), None))
            .unwrap()
            .entries
            .len(),
        1
    );
    assert_eq!(
        service
            .co_changed(&query("a.rs", None, Some(500)))
            .unwrap()
            .entries
            .len(),
        0
    );
    assert!(matches!(
        service.co_changed(&query("a.rs", Some(1), None)),
        Err(GitQueryError::InvalidQuery(_))
    ));
}

#[tokio::test]
async fn shallow_history_is_reported_as_partial_coverage() {
    let repo = TestRepo::new();
    let mut ids = Vec::new();
    for n in 0..5 {
        repo.write("a.rs", &format!("{n}\n"));
        ids.push(repo.commit("c", ALICE, START + n * DAY));
    }
    std::fs::write(repo.path().join(".git/shallow"), format!("{}\n", ids[2])).unwrap();
    let snapshot = refresh(&service(&repo)).await;
    assert!(snapshot.coverage.shallow);
    assert_eq!(snapshot.coverage.commits_read, 3);
    assert_eq!(snapshot.coverage.walk_stop, WalkStop::Exhausted);
}

#[tokio::test]
async fn window_limits_apply_by_commit_count_and_by_age() {
    let repo = TestRepo::new();
    for (n, days) in [0, 100, 200, 300].into_iter().enumerate() {
        repo.write("a.rs", &format!("{n}\n"));
        repo.commit("c", ALICE, START + days * DAY);
    }
    // Newest first: times 300, 200, 100, 0 days.
    let by_count = refresh(&service_with(
        &repo,
        GitAnalysisConfig {
            max_commits: 2,
            ..GitAnalysisConfig::default()
        },
    ))
    .await;
    assert_eq!(
        (by_count.coverage.commits_read, by_count.coverage.walk_stop),
        (2, WalkStop::CommitLimit)
    );
    let by_age = refresh(&service(&repo)).await;
    assert_eq!(
        (by_age.coverage.commits_read, by_age.coverage.walk_stop),
        (2, WalkStop::AgeLimit)
    );
    assert_eq!(
        by_age.coverage.earliest_commit_time,
        Some(START + 200 * DAY)
    );
    let all = refresh(&service_with(
        &repo,
        GitAnalysisConfig {
            max_age_days: 400,
            ..GitAnalysisConfig::default()
        },
    ))
    .await;
    assert_eq!(
        (all.coverage.commits_read, all.coverage.walk_stop),
        (4, WalkStop::Exhausted)
    );
}

#[tokio::test]
async fn incremental_refresh_equals_a_full_recompute() {
    let repo = TestRepo::new();
    let mut time = START;
    let mut change = |repo: &TestRepo, files: &[&str]| {
        for f in files {
            let existing = std::fs::read_to_string(repo.path().join(f)).unwrap_or_default();
            repo.write(f, &format!("{existing}x\n"));
        }
        time += DAY;
        repo.commit("change", BOB, time);
    };
    for _ in 0..4 {
        change(&repo, &["a.rs", "b.rs"]);
    }
    let source = RecordingSource::new(repo.path());
    let incremental = GitAnalysisBuilder::new(source.clone()).build().unwrap();
    let first = refresh(&incremental).await;
    assert!(source
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|r| r.stop_at.is_none()));

    for _ in 0..3 {
        change(&repo, &["a.rs", "b.rs", "c.rs"]);
    }
    let second = refresh(&incremental).await;
    assert_ne!(first.id, second.id);
    let requests = source.requests.lock().unwrap().clone();
    assert!(
        requests.last().unwrap().stop_at.is_some(),
        "the second refresh read only new commits"
    );

    let fresh = refresh(&service(&repo)).await;
    assert_eq!(second.index, fresh.index);
    assert_eq!(second.coverage, fresh.coverage);
    assert_eq!(second.id, fresh.id);
}

#[tokio::test]
async fn incremental_refresh_matches_full_when_the_window_slides() {
    let repo = TestRepo::new();
    let mut time = START;
    for n in 0..6 {
        repo.write("a.rs", &format!("{n}\n"));
        time += 40 * DAY;
        repo.commit("c", ALICE, time);
    }
    let config = GitAnalysisConfig {
        max_commits: 4,
        ..GitAnalysisConfig::default()
    };
    let source = RecordingSource::new(repo.path());
    let incremental = GitAnalysisBuilder::new(source)
        .config(config.clone())
        .build()
        .unwrap();
    refresh(&incremental).await;
    for n in 6..9 {
        repo.write("a.rs", &format!("{n}\n"));
        time += 40 * DAY;
        repo.commit("c", ALICE, time);
    }
    let slid = refresh(&incremental).await;
    let fresh = refresh(&service_with(&repo, config)).await;
    assert_eq!(slid.coverage, fresh.coverage);
    assert_eq!(slid.index, fresh.index);
}

#[tokio::test]
async fn refresh_reuses_the_snapshot_until_head_config_or_state_changes() {
    let repo = TestRepo::new();
    repo.write("a.rs", "1\n");
    repo.commit("c1", ALICE, START);
    let service = service(&repo);
    let first = refresh(&service).await;
    let again = refresh(&service).await;
    assert_eq!(first.id, again.id);
    assert!(
        Arc::ptr_eq(&first, &again),
        "an unchanged refresh returns the published snapshot"
    );

    // An uncommitted edit is reported, not analysed, and is part of the snapshot's state.
    repo.write("a.rs", "dirty\n");
    let dirty = refresh(&service).await;
    assert!(dirty.coverage.working_tree_dirty);
    assert_ne!(dirty.id, first.id);
    assert_eq!(dirty.index, first.index);

    repo.write("a.rs", "2\n");
    repo.commit("c2", ALICE, START + DAY);
    let moved = refresh(&service).await;
    assert_ne!(moved.id, dirty.id);
    assert_ne!(moved.head, first.head);
    assert!(!moved.coverage.working_tree_dirty);

    let other_config = GitAnalysisConfig {
        min_support: 5,
        ..GitAnalysisConfig::default()
    };
    assert_ne!(
        refresh(&self::service_with(&repo, other_config)).await.id,
        moved.id
    );
    // Older snapshots still answer with their own data.
    assert_eq!(service.get(&first.id).unwrap().head, first.head);
}

#[tokio::test]
async fn cancelled_refresh_keeps_the_previous_snapshot() {
    let repo = TestRepo::new();
    repo.write("a.rs", "1\n");
    repo.commit("c1", ALICE, START);
    let service = service(&repo);
    let first = refresh(&service).await;
    repo.write("a.rs", "2\n");
    repo.commit("c2", ALICE, START + DAY);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = service
        .refresh(RepositoryCorrelation::default(), cancel)
        .await;
    assert!(matches!(result, Err(GitAnalysisError::Cancelled)));
    assert_eq!(service.current().unwrap().id, first.id);
}

#[tokio::test]
async fn queries_page_sort_and_reject_bad_input() {
    let repo = TestRepo::new();
    for (n, name) in ["a.rs", "b.rs", "c.rs", "src/d.rs"].into_iter().enumerate() {
        for round in 0..=n {
            repo.write(name, &format!("{round}\n"));
            repo.commit("c", ALICE, START + (n * 10 + round) as i64 * DAY / 10);
        }
    }
    let service = service(&repo);
    let snapshot = refresh(&service).await;
    let id = &snapshot.id;

    let by_commits = service
        .churn(&churn_query(
            id,
            ChurnScope::Repository,
            ChurnSort::Commits,
            10,
        ))
        .unwrap();
    assert_eq!(
        by_commits
            .entries
            .iter()
            .map(|e| e.commits)
            .collect::<Vec<_>>(),
        vec![4, 3, 2, 1]
    );
    let prefix = service
        .churn(&churn_query(
            id,
            ChurnScope::Prefix(path("src")),
            ChurnSort::Path,
            10,
        ))
        .unwrap();
    assert_eq!(prefix.entries.len(), 1);
    let one = service
        .churn(&churn_query(
            id,
            ChurnScope::File(path("b.rs")),
            ChurnSort::Path,
            10,
        ))
        .unwrap();
    assert_eq!(one.entries[0].commits, 2);

    // Paging returns every entry exactly once.
    let mut collected = Vec::new();
    let mut query = churn_query(id, ChurnScope::Repository, ChurnSort::Path, 1);
    loop {
        let page = service.churn(&query).unwrap();
        collected.extend(page.entries);
        match page.next_cursor {
            Some(next) => query.cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(collected.len(), 4);

    let first_page = service
        .churn(&churn_query(id, ChurnScope::Repository, ChurnSort::Path, 1))
        .unwrap();
    let cursor = first_page.next_cursor.unwrap();
    let mut foreign = cursor.clone();
    foreign.snapshot = GitSnapshotId("other".into());
    let mut with_foreign = churn_query(id, ChurnScope::Repository, ChurnSort::Path, 1);
    with_foreign.cursor = Some(foreign);
    assert_eq!(
        service.churn(&with_foreign).unwrap_err(),
        GitQueryError::ForeignCursor
    );
    let mut mismatched = churn_query(id, ChurnScope::Repository, ChurnSort::Commits, 1);
    mismatched.cursor = Some(cursor);
    assert_eq!(
        service.churn(&mismatched).unwrap_err(),
        GitQueryError::InvalidCursor
    );
    assert!(matches!(
        service.churn(&churn_query(id, ChurnScope::Repository, ChurnSort::Path, 0)),
        Err(GitQueryError::InvalidLimit { .. })
    ));
    assert!(matches!(
        service.churn(&churn_query(
            id,
            ChurnScope::File(path("nope.rs")),
            ChurnSort::Path,
            1
        )),
        Err(GitQueryError::UnknownFile(_))
    ));
    assert!(matches!(
        service.churn(&churn_query(
            &GitSnapshotId("gone".into()),
            ChurnScope::Repository,
            ChurnSort::Path,
            1
        )),
        Err(GitQueryError::SnapshotNotFound(_))
    ));
    assert!(matches!(
        service.coverage(&GitSnapshotId("gone".into())),
        Err(GitQueryError::SnapshotNotFound(_))
    ));
}

#[tokio::test]
async fn coverage_joins_git_facts_to_the_repository_snapshot() {
    let repo = TestRepo::new();
    repo.write("a.rs", "1\n");
    repo.write("b.rs", "1\n");
    repo.write("README.md", "docs\n");
    repo.commit("c1", ALICE, START);
    let known = ["a.rs", "unchanged.rs"]
        .iter()
        .map(|p| path(p))
        .collect::<BTreeSet<_>>();
    let correlation = RepositoryCorrelation {
        snapshot: Some(SnapshotId("repo-1".into())),
        known_files: known,
    };
    let service = service(&repo);
    let snapshot = service
        .refresh(correlation, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        snapshot.repository_snapshot,
        Some(SnapshotId("repo-1".into()))
    );
    assert_eq!(snapshot.coverage.not_in_repository_snapshot, 2);
    assert_eq!(
        snapshot.coverage.not_in_repository_snapshot_sample,
        vec![path("README.md"), path("b.rs")]
    );
    assert_eq!(snapshot.coverage.repository_files_without_history, 1);
    // Joining to a different repository snapshot is a different Git snapshot.
    let other = service
        .refresh(
            RepositoryCorrelation {
                snapshot: Some(SnapshotId("repo-2".into())),
                known_files: BTreeSet::new(),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_ne!(other.id, snapshot.id);
}

#[tokio::test]
async fn a_subdirectory_root_reports_paths_relative_to_it() {
    let repo = TestRepo::new();
    repo.write("outside.rs", "1\n");
    repo.write("project/src/lib.rs", "1\n");
    repo.commit("c1", ALICE, START);
    let service = GitAnalysisBuilder::for_repository(repo.path().join("project"))
        .build()
        .unwrap();
    let snapshot = refresh(&service).await;
    let files = snapshot
        .index
        .churn
        .iter()
        .map(|c| c.file.as_path().to_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(files, vec!["src/lib.rs"]);
}

#[tokio::test]
async fn binary_changes_have_no_line_counts() {
    let repo = TestRepo::new();
    repo.write_bytes("logo.png", &[0, 159, 146, 150, 0, 1, 2]);
    repo.write("a.rs", "x\n");
    repo.commit("add", ALICE, START);
    let snapshot = refresh(&service(&repo)).await;
    let logo = snapshot
        .index
        .churn
        .iter()
        .find(|c| c.file == path("logo.png"))
        .unwrap();
    assert_eq!((logo.lines_added, logo.changes_without_lines), (0, 1));
    assert_eq!(snapshot.coverage.changes_without_lines, 1);
}

#[tokio::test]
async fn analysis_is_deterministic_across_services_and_runs() {
    let repo = TestRepo::new();
    for n in 0..6 {
        repo.write("a.rs", &format!("{n}\n"));
        repo.write(&format!("f{}.rs", n % 3), &format!("{n}\n"));
        repo.commit("c", if n % 2 == 0 { ALICE } else { BOB }, START + n * DAY);
    }
    let first = refresh(&service(&repo)).await;
    let second = refresh(&service(&repo)).await;
    assert_eq!(first.id, second.id);
    assert_eq!(first.index, second.index);
}

#[tokio::test]
async fn invalid_configuration_is_rejected_when_building() {
    let repo = TestRepo::new();
    for config in [
        GitAnalysisConfig {
            max_commits: 0,
            ..GitAnalysisConfig::default()
        },
        GitAnalysisConfig {
            max_commits: 20_000,
            ..GitAnalysisConfig::default()
        },
        GitAnalysisConfig {
            min_support: 0,
            ..GitAnalysisConfig::default()
        },
        GitAnalysisConfig {
            max_files_per_commit: 1,
            ..GitAnalysisConfig::default()
        },
    ] {
        let built = GitAnalysisBuilder::for_repository(repo.path())
            .config(config)
            .build();
        assert!(matches!(built, Err(GitAnalysisError::InvalidConfig(_))));
    }
}

#[tokio::test]
async fn a_directory_without_a_repository_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let service = GitAnalysisBuilder::for_repository(dir.path())
        .build()
        .unwrap();
    let result = service
        .refresh(RepositoryCorrelation::default(), CancellationToken::new())
        .await;
    assert!(matches!(result, Err(GitAnalysisError::NotARepository(_))));
}
