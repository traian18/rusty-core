use std::fs;
use std::time::{Duration, SystemTime};

use super::scan::scan_at;
use super::{apply, Kind, Policy};

fn days_ahead(n: u64) -> SystemTime {
    SystemTime::now() + Duration::from_secs(n * 86_400)
}

#[test]
fn a_scan_reports_old_leftovers_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join(".harness/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("a.jsonl"), vec![b'x'; 2048]).unwrap();
    let worktree = dir.path().join(".harness-worktrees/branch");
    fs::create_dir_all(&worktree).unwrap();
    fs::write(worktree.join("file.txt"), "hello").unwrap();

    let policy = Policy::default();
    // Nothing is old yet.
    assert!(scan_at(dir.path(), &policy, SystemTime::now())
        .candidates
        .is_empty());

    // Forty days later both are, the worktree after a week and the session after a month.
    let report = scan_at(dir.path(), &policy, days_ahead(40));
    assert_eq!(report.candidates.len(), 2);
    assert!(report
        .candidates
        .iter()
        .any(|c| c.kind == Kind::Session && c.bytes == 2048));
    assert!(report.candidates.iter().any(|c| c.kind == Kind::Worktree));
    assert!(report.render().contains("2 item(s)"));
    assert!(
        sessions.join("a.jsonl").exists(),
        "scanning removes nothing"
    );

    // Ten days later only the worktree is.
    let report = scan_at(dir.path(), &policy, days_ahead(10));
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].kind, Kind::Worktree);
}

#[test]
fn applying_removes_exactly_the_reported_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join(".harness/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("old.jsonl"), "old").unwrap();
    fs::write(sessions.join("other.jsonl"), "other").unwrap();

    let mut report = scan_at(dir.path(), &Policy::default(), days_ahead(40));
    report.candidates.retain(|c| c.path.ends_with("old.jsonl"));
    let removed = apply(dir.path(), &report);

    assert_eq!(removed.items, 1);
    assert!(removed.failed.is_empty());
    assert!(!sessions.join("old.jsonl").exists());
    assert!(sessions.join("other.jsonl").exists());
}
