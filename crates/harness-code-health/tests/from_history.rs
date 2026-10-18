//! From a scripted Git history to history findings, through the real Git analysis.
use git2::{IndexAddOption, Repository, Signature, Time};
use harness_code_analysis::{metrics::measure_file, LanguageId};
use harness_code_health::{detect_file, DetectorConfig, FileContext, Severity};
use harness_git_analysis::{GitAnalysis, GitAnalysisBuilder, RepositoryCorrelation};
use tokio_util::sync::CancellationToken;

const DAY: i64 = 86_400;
const T: i64 = 1_700_000_000;

fn commit(repo: &Repository, message: &str, who: &str, time: i64) {
    let mut index = repo.index().unwrap();
    index.add_all(["."], IndexAddOption::DEFAULT, None).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature =
        Signature::new(who, &format!("{who}@example.com"), &Time::new(time, 0)).unwrap();
    let parents: Vec<_> = repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_commit().ok())
        .into_iter()
        .collect();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parents.iter().collect::<Vec<_>>(),
    )
    .unwrap();
}

fn body(lines: usize) -> String {
    (0..lines)
        .map(|i| format!("pub fn f{i}() {{}}\n"))
        .collect()
}

#[tokio::test]
async fn a_file_fixed_again_and_again_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let write = |name: &str, text: &str| std::fs::write(dir.path().join(name), text).unwrap();
    for name in ["hot.rs", "a.rs", "b.rs", "c.rs", "d.rs"] {
        write(name, &body(5));
    }
    commit(&repo, "initial", "alice", T - 30 * DAY);
    for (i, message) in ["fix the parser crash", "Fix empty input", "fixes #12"]
        .iter()
        .enumerate()
    {
        write("hot.rs", &body(35 + 30 * i));
        commit(&repo, message, "alice", T - (20 - i as i64 * 5) * DAY);
    }
    let git = GitAnalysisBuilder::for_repository(dir.path())
        .build()
        .unwrap();
    let snapshot = git
        .refresh(RepositoryCorrelation::default(), CancellationToken::new())
        .await
        .unwrap();
    let path = harness_workspace::repository_intelligence::RootRelativePath::new("hot.rs").unwrap();
    let signals = snapshot
        .index
        .signals
        .iter()
        .find(|s| s.file == path)
        .unwrap();
    assert_eq!(
        (signals.fix_commits, signals.fix_percentile_per_mille),
        (3, 800)
    );
    assert!(signals.hotspot);

    let source = std::fs::read(dir.path().join("hot.rs")).unwrap();
    let metrics = measure_file(&LanguageId::Rust, "hot.rs", &source).unwrap();
    let context =
        FileContext::new("hot.rs", &metrics).with_history(signals, &snapshot.index.signal_summary);
    let findings = detect_file(&context, &DetectorConfig::default());
    let prior = findings
        .iter()
        .find(|f| f.detector == "prior_defect")
        .expect("prior_defect");
    assert_eq!(
        prior.severity,
        Severity::Critical,
        "three fixes on a hotspot"
    );
    assert!(prior
        .reason
        .contains("Bug-fixed 3 times in the last 180 days"));
    // One author, one person on the team: no ownership or knowledge findings.
    assert!(!findings
        .iter()
        .any(|f| f.detector == "ownership_risk" || f.detector == "knowledge_loss"));
}
