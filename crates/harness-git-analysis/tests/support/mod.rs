#![allow(dead_code)]
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use git2::{IndexAddOption, Oid, Repository, Signature, Time};
use harness_git_analysis::{
    CommitBatch, CommitId, GitAnalysis, GitAnalysisBuilder, GitAnalysisConfig, GitAnalysisError,
    GitAnalysisService, GitHistorySource, GitSnapshot, HeadInfo, HistoryRequest,
    Libgit2HistorySource, RepositoryCorrelation,
};
use harness_workspace::repository_intelligence::RootRelativePath;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

pub const DAY: i64 = 86_400;
pub const START: i64 = 1_700_000_000;

/// A throwaway repository whose commit times and authors are fully controlled.
pub struct TestRepo {
    pub dir: TempDir,
    pub repo: Repository,
}

impl TestRepo {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        Self { dir, repo }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn write(&self, relative: &str, content: &str) {
        let full = self.path().join(relative);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }

    pub fn write_bytes(&self, relative: &str, content: &[u8]) {
        let full = self.path().join(relative);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }

    pub fn remove(&self, relative: &str) {
        std::fs::remove_file(self.path().join(relative)).unwrap();
    }

    pub fn rename(&self, from: &str, to: &str) {
        let target = self.path().join(to);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::rename(self.path().join(from), target).unwrap();
    }

    /// Commits the whole working tree as `who` at `time`.
    pub fn commit(&self, message: &str, who: (&str, &str), time: i64) -> Oid {
        self.commit_with_parents(message, who, time, &[])
    }

    /// A merge commit: HEAD plus `extra` parents, with the current working tree.
    pub fn merge(&self, message: &str, who: (&str, &str), time: i64, extra: Oid) -> Oid {
        self.commit_with_parents(message, who, time, &[extra])
    }

    fn commit_with_parents(
        &self,
        message: &str,
        who: (&str, &str),
        time: i64,
        extra: &[Oid],
    ) -> Oid {
        let mut index = self.repo.index().unwrap();
        index.add_all(["."], IndexAddOption::DEFAULT, None).unwrap();
        index.update_all(["."], None).unwrap();
        index.write().unwrap();
        let tree = self.repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = Signature::new(who.0, who.1, &Time::new(time, 0)).unwrap();
        let mut parents = Vec::new();
        if let Ok(head) = self.repo.head().and_then(|h| h.peel_to_commit()) {
            parents.push(head);
        }
        for oid in extra {
            parents.push(self.repo.find_commit(*oid).unwrap());
        }
        let parent_refs = parents.iter().collect::<Vec<_>>();
        self.repo
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                message,
                &tree,
                &parent_refs,
            )
            .unwrap()
    }

    pub fn head(&self) -> Oid {
        self.repo.head().unwrap().peel_to_commit().unwrap().id()
    }
}

pub const ALICE: (&str, &str) = ("Alice", "alice@example.com");
pub const BOB: (&str, &str) = ("Bob", "bob@example.com");

pub fn path(value: &str) -> RootRelativePath {
    RootRelativePath::new(value).unwrap()
}

pub fn service(repo: &TestRepo) -> GitAnalysisService {
    service_with(repo, GitAnalysisConfig::default())
}

pub fn service_with(repo: &TestRepo, config: GitAnalysisConfig) -> GitAnalysisService {
    GitAnalysisBuilder::for_repository(repo.path())
        .config(config)
        .build()
        .unwrap()
}

pub async fn refresh(service: &impl GitAnalysis) -> Arc<GitSnapshot> {
    service
        .refresh(RepositoryCorrelation::default(), CancellationToken::new())
        .await
        .unwrap()
}

/// Delegates to libgit2 and remembers every history request.
pub struct RecordingSource {
    inner: Libgit2HistorySource,
    pub requests: Mutex<Vec<HistoryRequest>>,
}

impl RecordingSource {
    pub fn new(root: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            inner: Libgit2HistorySource::new(root),
            requests: Mutex::new(Vec::new()),
        })
    }
}

impl GitHistorySource for RecordingSource {
    fn head(&self) -> Result<HeadInfo, GitAnalysisError> {
        self.inner.head()
    }
    fn commits(
        &self,
        request: &HistoryRequest,
        cancel: &CancellationToken,
    ) -> Result<CommitBatch, GitAnalysisError> {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.commits(request, cancel)
    }
    fn is_ancestor(
        &self,
        ancestor: &CommitId,
        descendant: &CommitId,
    ) -> Result<bool, GitAnalysisError> {
        self.inner.is_ancestor(ancestor, descendant)
    }
    fn present_at_head(
        &self,
        paths: &[RootRelativePath],
    ) -> Result<std::collections::BTreeSet<RootRelativePath>, GitAnalysisError> {
        self.inner.present_at_head(paths)
    }
}
