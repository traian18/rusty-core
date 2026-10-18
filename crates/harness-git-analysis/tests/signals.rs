//! Windowed per-file signals on scripted repositories whose times, authors and messages are fixed.
mod support;

use harness_git_analysis::{FileSignals, GitSnapshot, SIGNALS_VERSION};
use support::*;

const CAROL: (&str, &str) = ("Carol", "carol@example.com");
/// The newest commit of each scenario; every window ends here.
const T: i64 = START + 200 * DAY;

fn lines(n: usize) -> String {
    (0..n).map(|i| format!("line {i}\n")).collect()
}

fn signals<'a>(snapshot: &'a GitSnapshot, file: &str) -> &'a FileSignals {
    snapshot
        .index
        .signals
        .iter()
        .find(|s| s.file == path(file))
        .unwrap_or_else(|| panic!("no signals for {file}"))
}

/// hot.rs: three recent edits by three people, two of them fixes; calm.rs: an old fix;
/// a.rs and b.rs: one scattered commit together; c.rs: only the initial import.
fn scenario() -> TestRepo {
    let repo = TestRepo::new();
    repo.write("hot.rs", &lines(10));
    repo.write("calm.rs", &lines(10));
    for name in ["a.rs", "b.rs", "c.rs"] {
        repo.write(name, "x\n");
    }
    // Older than the fix window: its "fix" must not count.
    repo.commit("fix: initial import", ALICE, T - 181 * DAY);
    repo.write("calm.rs", &lines(11));
    repo.commit("Fix calm edge case", BOB, T - 120 * DAY);
    repo.write("hot.rs", &lines(40));
    repo.commit("fix crash in hot path", ALICE, T - 60 * DAY);
    repo.write("hot.rs", &lines(70));
    repo.commit("add feature", BOB, T - 20 * DAY);
    repo.write("a.rs", "x\ny\n");
    repo.write("b.rs", "x\ny\n");
    repo.commit("tidy", BOB, T - 5 * DAY);
    repo.write("hot.rs", &lines(100));
    repo.commit("Fixes #3", CAROL, T);
    repo
}

#[tokio::test]
async fn windows_fixes_decayed_churn_and_the_hotspot_rule() {
    let repo = scenario();
    let snapshot = refresh(&service(&repo)).await;
    let summary = &snapshot.index.signal_summary;
    assert_eq!(
        (
            summary.version,
            summary.as_of,
            summary.recent_days,
            summary.fix_window_days
        ),
        (SIGNALS_VERSION, Some(T), 90, 180)
    );
    assert_eq!((summary.fix_commits, summary.hotspots), (3, 1));

    let hot = signals(&snapshot, "hot.rs");
    assert_eq!(
        (hot.commits, hot.commits_recent, hot.lines_changed_recent),
        (4, 3, 90)
    );
    assert_eq!(
        hot.fix_commits, 2,
        "the initial import is older than 180 days"
    );
    assert_eq!(hot.fix_examples.len(), 2);
    // 10 lines at 181 days + 30 lines at 60, 20 and 0 days, halved every 180 days, per 100 lines.
    assert!(
        (860..=870).contains(&hot.decayed_churn_milli),
        "{}",
        hot.decayed_churn_milli
    );
    assert_eq!(hot.churn_percentile_per_mille, 800);
    assert!(hot.hotspot);
    assert_eq!((hot.age_days, hot.idle_days), (181, 0));

    let calm = signals(&snapshot, "calm.rs");
    assert_eq!(
        (calm.fix_commits, calm.commits_recent, calm.hotspot),
        (1, 0, false)
    );
    assert_eq!(calm.idle_days, 120);
}

#[tokio::test]
async fn change_entropy_rewards_scattered_commits() {
    let repo = scenario();
    let snapshot = refresh(&service(&repo)).await;
    // The initial import spreads 23 lines over five files (normalised entropy 0.704); the tidy
    // commit splits two lines evenly over a.rs and b.rs (1.0). Single-file commits add nothing.
    let (a, c, hot) = (
        signals(&snapshot, "a.rs"),
        signals(&snapshot, "c.rs"),
        signals(&snapshot, "hot.rs"),
    );
    assert_eq!(c.change_entropy_milli, 704);
    assert_eq!(hot.change_entropy_milli, 704);
    assert_eq!(a.change_entropy_milli, 1704);
    assert_eq!(a.change_entropy_percentile_per_mille, 600);
    assert_eq!(c.change_entropy_percentile_per_mille, 0);
}

#[tokio::test]
async fn ownership_shares_minor_contributors_and_bus_factor() {
    let repo = scenario();
    let snapshot = refresh(&service(&repo)).await;
    let hot = signals(&snapshot, "hot.rs");
    assert_eq!(hot.contributors, 3);
    assert_eq!(hot.owner.as_ref().map(|o| o.name.as_str()), Some("Alice"));
    assert_eq!(hot.owner_share_per_mille, 500);
    assert_eq!(hot.recent_owner_share_per_mille, 333);
    assert_eq!((hot.bus_factor, hot.minor_contributors), (1, 0));

    let repo = TestRepo::new();
    for i in 0..20 {
        repo.write("owned.rs", &lines(i + 1));
        repo.commit("work", ALICE, T - (40 - i as i64) * DAY);
    }
    repo.write("owned.rs", &lines(30));
    repo.commit("drive-by", BOB, T);
    let snapshot = refresh(&service(&repo)).await;
    let owned = signals(&snapshot, "owned.rs");
    assert_eq!(
        (
            owned.owner_share_per_mille,
            owned.minor_contributors,
            owned.bus_factor
        ),
        (952, 1, 1)
    );
    assert_eq!(
        owned.recent_owner.as_ref().map(|o| o.name.as_str()),
        Some("Alice")
    );
}

#[tokio::test]
async fn a_quiet_repository_has_no_hotspots_but_steady_activity_does() {
    // Every file changed once or twice: the top quartile exists, but nothing changes often.
    let repo = TestRepo::new();
    for name in ["a.rs", "b.rs", "c.rs", "d.rs"] {
        repo.write(name, &lines(200));
    }
    repo.commit("import", ALICE, T - 10 * DAY);
    repo.write("a.rs", &lines(400));
    repo.commit("grow a", ALICE, T);
    let snapshot = refresh(&service(&repo)).await;
    assert_eq!(snapshot.index.signal_summary.hotspots, 0);
    assert!(
        signals(&snapshot, "a.rs").decayed_churn_milli >= 500,
        "big enough, but only two commits"
    );

    // Eight one-line commits in the recent window clear the floor through the commit count.
    let repo = TestRepo::new();
    for name in ["busy.rs", "x.rs", "y.rs", "z.rs"] {
        repo.write(name, "0\n");
    }
    repo.commit("import", ALICE, T - 30 * DAY);
    for i in 0..8 {
        repo.write("busy.rs", &lines(i + 2));
        repo.commit("tweak", ALICE, T - (8 - i as i64) * DAY);
    }
    let snapshot = refresh(&service(&repo)).await;
    let busy = signals(&snapshot, "busy.rs");
    assert!(busy.decayed_churn_milli < 500);
    assert!(busy.hotspot);
}

#[tokio::test]
async fn fixes_follow_renames_and_merges_are_not_counted() {
    let repo = TestRepo::new();
    repo.write("old.rs", &lines(3));
    let first = repo.commit("add", ALICE, T - 10 * DAY);
    repo.write("old.rs", &lines(4));
    repo.commit("fix overflow", ALICE, T - 9 * DAY);
    repo.rename("old.rs", "new.rs");
    repo.commit("rename", ALICE, T - 8 * DAY);
    // A merge says "fix" but changes nothing of its own.
    repo.merge("fix everything", BOB, T, first);
    let snapshot = refresh(&service(&repo)).await;
    let renamed = signals(&snapshot, "new.rs");
    assert_eq!((renamed.commits, renamed.fix_commits), (3, 1));
    assert_eq!(snapshot.index.signal_summary.fix_commits, 1);
    assert!(snapshot
        .index
        .signals
        .iter()
        .all(|s| s.file != path("old.rs")));
}

#[tokio::test]
async fn signals_are_deterministic() {
    let repo = scenario();
    let first = refresh(&service(&repo)).await;
    let second = refresh(&service(&repo)).await;
    assert_eq!(first.index.signals, second.index.signals);
    assert_eq!(first.id, second.id);
}

#[tokio::test]
async fn fix_rank_recent_authors_and_co_change_partners() {
    let repo = scenario();
    let snapshot = refresh(&service(&repo)).await;
    let (hot, calm, a, c) = (
        signals(&snapshot, "hot.rs"),
        signals(&snapshot, "calm.rs"),
        signals(&snapshot, "a.rs"),
        signals(&snapshot, "c.rs"),
    );
    // Fix commits 2, 1, 0, 0, 0 across five files.
    assert_eq!(
        (
            hot.fix_percentile_per_mille,
            calm.fix_percentile_per_mille,
            c.fix_percentile_per_mille
        ),
        (800, 600, 0)
    );
    assert_eq!(
        (hot.recent_contributors, hot.owner_recent_share_per_mille),
        (3, 333)
    );
    assert_eq!(calm.owner_recent_share_per_mille, 0, "no recent commits");
    assert_eq!(snapshot.index.signal_summary.active_contributors_recent, 3);
    // a.rs and b.rs changed together twice (the import and the tidy); everything else once.
    assert_eq!(
        (a.co_change_partners, a.co_change_percentile_per_mille),
        (1, 600)
    );
    assert_eq!(
        (hot.co_change_partners, hot.co_change_percentile_per_mille),
        (0, 0)
    );
}
