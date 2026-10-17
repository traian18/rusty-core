mod support;

use std::{collections::BTreeSet, sync::Arc};

use harness_git_analysis::{
    ChangeKind, CommitBatch, CommitFact, CommitId, Contributor, ContributorId, FileChange,
    GitAnalysis, GitAnalysisBuilder, GitAnalysisConfig, GitAnalysisError, GitHistorySource,
    HeadInfo, HistoryRequest, RepositoryCorrelation, WalkStop,
};
use harness_workspace::repository_intelligence::RootRelativePath;
use support::*;
use tokio_util::sync::CancellationToken;

/// A history source that never touches Git, to prove the analysis depends only on the contract.
struct FakeSource {
    commits: Vec<CommitFact>,
}

fn change(file: &str, kind: ChangeKind, previous: Option<&str>) -> FileChange {
    FileChange {
        path: path(file),
        previous_path: previous.map(path),
        kind,
        lines_added: Some(2),
        lines_removed: Some(1),
    }
}

fn commit(id: &str, time: i64, changes: Vec<FileChange>) -> CommitFact {
    CommitFact {
        id: CommitId(id.into()),
        parent_count: 1,
        author_time: time,
        contributor: Contributor {
            id: ContributorId("c1".into()),
            name: "Carol".into(),
        },
        changes,
    }
}

impl GitHistorySource for FakeSource {
    fn head(&self) -> Result<HeadInfo, GitAnalysisError> {
        Ok(HeadInfo {
            id: self.commits[0].id.clone(),
            author_time: self.commits[0].author_time,
            shallow: false,
            working_tree_dirty: false,
        })
    }
    fn commits(
        &self,
        _: &HistoryRequest,
        _: &CancellationToken,
    ) -> Result<CommitBatch, GitAnalysisError> {
        Ok(CommitBatch {
            examined: self.commits.len(),
            commits: self.commits.clone(),
            stop: WalkStop::Exhausted,
        })
    }
    fn is_ancestor(&self, _: &CommitId, _: &CommitId) -> Result<bool, GitAnalysisError> {
        Ok(false)
    }
    fn present_at_head(
        &self,
        paths: &[RootRelativePath],
    ) -> Result<BTreeSet<RootRelativePath>, GitAnalysisError> {
        Ok(paths.iter().cloned().collect())
    }
}

#[tokio::test]
async fn analysis_runs_on_any_history_source() {
    let source = FakeSource {
        commits: vec![
            commit(
                "c4",
                400,
                vec![
                    change("b.rs", ChangeKind::Modified, None),
                    change("a.rs", ChangeKind::Modified, None),
                ],
            ),
            commit(
                "c3",
                300,
                vec![
                    change("b.rs", ChangeKind::Modified, None),
                    change("a.rs", ChangeKind::Modified, None),
                ],
            ),
            commit(
                "c2",
                200,
                vec![
                    change("b.rs", ChangeKind::Renamed, Some("a_old.rs")),
                    change("a.rs", ChangeKind::Modified, None),
                ],
            ),
            commit("c1", 100, vec![change("a_old.rs", ChangeKind::Added, None)]),
        ],
    };
    let config = GitAnalysisConfig {
        min_support: 2,
        ..GitAnalysisConfig::default()
    };
    let service = GitAnalysisBuilder::new(Arc::new(source))
        .config(config)
        .build()
        .unwrap();
    let snapshot = service
        .refresh(RepositoryCorrelation::default(), CancellationToken::new())
        .await
        .unwrap();
    let b = snapshot
        .index
        .churn
        .iter()
        .find(|c| c.file == path("b.rs"))
        .unwrap();
    assert_eq!(b.commits, 4, "a_old.rs history is followed into b.rs");
    let partners = snapshot
        .index
        .co_changes
        .iter()
        .filter(|c| c.file == path("b.rs"))
        .collect::<Vec<_>>();
    assert_eq!(partners.len(), 1);
    assert_eq!(partners[0].partner, path("a.rs"));
    assert_eq!(partners[0].support, 3);
    assert_eq!(
        partners[0].example_commits,
        vec![
            CommitId("c4".into()),
            CommitId("c3".into()),
            CommitId("c2".into())
        ]
    );
    assert_eq!(snapshot.coverage.commits_used, 4);
}
