//! Reproducible Git-analysis timings on a generated repository: cold refresh, unchanged refresh,
//! incremental refresh after new commits, and bounded queries.
//!
//! Run with `cargo bench -p harness-git-analysis --bench git_analysis`. Raw evidence for the
//! machine it ran on, not a service-level promise.
use std::time::Instant;

use git2::{IndexAddOption, Repository, Signature, Time};
use harness_git_analysis::{
    ChurnQuery, ChurnScope, ChurnSort, CoChangeQuery, GitAnalysis, GitAnalysisBuilder,
    RepositoryCorrelation,
};
use harness_workspace::repository_intelligence::RootRelativePath;
use tokio_util::sync::CancellationToken;

const FILES: usize = 120;
const COMMITS: usize = 500;
const NEW_COMMITS: usize = 20;
const QUERY_REPETITIONS: u32 = 2_000;

fn main() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(run());
}

fn commit(repo: &Repository, number: usize) {
    // Each commit edits three neighbouring files, so co-change pairs build up.
    let first = (number * 7) % FILES;
    for offset in 0..3 {
        let file = (first + offset) % FILES;
        let path = repo
            .workdir()
            .expect("workdir")
            .join(format!("src/m{}/f{file}.rs", file % 10));
        std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        let previous = std::fs::read_to_string(&path).unwrap_or_default();
        std::fs::write(&path, format!("{previous}// change {number}\n")).expect("write");
    }
    let mut index = repo.index().expect("index");
    index
        .add_all(["."], IndexAddOption::DEFAULT, None)
        .expect("stage");
    index.write().expect("write index");
    let tree = repo
        .find_tree(index.write_tree().expect("tree"))
        .expect("find tree");
    let signature = Signature::new(
        "Bench",
        "bench@example.com",
        &Time::new(1_700_000_000 + number as i64 * 600, 0),
    )
    .expect("signature");
    let parents = repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_commit().ok())
        .into_iter()
        .collect::<Vec<_>>();
    let parent_refs = parents.iter().collect::<Vec<_>>();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "change",
        &tree,
        &parent_refs,
    )
    .expect("commit");
}

async fn run() {
    let dir = std::env::temp_dir().join(format!("git-analysis-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("workspace");
    let repo = Repository::init(&dir).expect("init");
    let built = Instant::now();
    for number in 0..COMMITS {
        commit(&repo, number);
    }
    let build_time = built.elapsed();
    let service = GitAnalysisBuilder::for_repository(&dir)
        .build()
        .expect("service");
    let refresh = || service.refresh(RepositoryCorrelation::default(), CancellationToken::new());

    let timed = Instant::now();
    let cold = refresh().await.expect("cold refresh");
    let cold_time = timed.elapsed();
    let timed = Instant::now();
    refresh().await.expect("unchanged refresh");
    let unchanged_time = timed.elapsed();
    for number in COMMITS..COMMITS + NEW_COMMITS {
        commit(&repo, number);
    }
    let timed = Instant::now();
    let extended = refresh().await.expect("incremental refresh");
    let incremental_time = timed.elapsed();

    let file = RootRelativePath::new("src/m0/f0.rs").expect("path");
    let churn = ChurnQuery {
        snapshot: extended.id.clone(),
        scope: ChurnScope::Repository,
        sort: ChurnSort::Commits,
        cursor: None,
        limit: 50,
    };
    let co_change = CoChangeQuery {
        snapshot: extended.id.clone(),
        file,
        min_support: None,
        min_confidence_per_mille: None,
        cursor: None,
        limit: 20,
    };
    let timed = Instant::now();
    for _ in 0..QUERY_REPETITIONS {
        service.churn(&churn).expect("churn");
        service.co_changed(&co_change).expect("co-change");
    }
    let query_time = timed.elapsed() / (2 * QUERY_REPETITIONS);

    println!(
        "environment: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("fixture: {COMMITS} commits (built in {build_time:?}), {FILES} files, 3 files changed per commit");
    println!("config: {:?}", cold.config);
    println!(
        "analysed: {} files, {} co-change relations, {} commits used",
        cold.index.churn.len(),
        cold.index.co_changes.len(),
        cold.coverage.commits_used
    );
    println!("cold refresh:        {cold_time:?}");
    println!("unchanged refresh:   {unchanged_time:?}");
    println!(
        "incremental refresh: {incremental_time:?} (+{NEW_COMMITS} commits, window {} commits)",
        extended.coverage.commits_read
    );
    println!(
        "bounded query:       {query_time:?} per call (mean of {} calls)",
        2 * QUERY_REPETITIONS
    );
    let _ = std::fs::remove_dir_all(&dir);
}
