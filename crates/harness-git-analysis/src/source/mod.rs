//! libgit2-backed [`GitHistorySource`]. Reads local history only; never fetches or writes.
mod commit_diff;

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use git2::{Oid, Repository, Sort, StatusOptions};
use harness_workspace::repository_intelligence::RootRelativePath;
use tokio_util::sync::CancellationToken;

use crate::{
    contracts::{CommitBatch, GitHistorySource, HeadInfo, HistoryRequest, WalkStop},
    error::GitAnalysisError,
    model::{CommitFact, CommitId, Contributor, ContributorId},
};

/// Reads the repository containing `root`; paths are reported relative to `root`.
pub struct Libgit2HistorySource {
    root: PathBuf,
}

impl Libgit2HistorySource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The repository and `root`'s path inside its working directory.
    fn open(&self) -> Result<(Repository, PathBuf), GitAnalysisError> {
        let repo = Repository::discover(&self.root)
            .map_err(|error| GitAnalysisError::NotARepository(error.message().to_owned()))?;
        let workdir = repo
            .workdir()
            .ok_or_else(|| GitAnalysisError::NotARepository("bare repository".into()))?
            .canonicalize()
            .map_err(failed)?;
        let root = self.root.canonicalize().map_err(failed)?;
        let prefix = root
            .strip_prefix(&workdir)
            .map(Path::to_path_buf)
            .map_err(|_| {
                GitAnalysisError::NotARepository("root is outside the working directory".into())
            })?;
        Ok((repo, prefix))
    }
}

fn failed(error: impl std::fmt::Display) -> GitAnalysisError {
    GitAnalysisError::Failed(error.to_string())
}

fn oid_of(id: &CommitId) -> Result<Oid, GitAnalysisError> {
    Oid::from_str(&id.0).map_err(failed)
}

impl GitHistorySource for Libgit2HistorySource {
    fn head(&self) -> Result<HeadInfo, GitAnalysisError> {
        let (repo, _) = self.open()?;
        let head = repo
            .head()
            .and_then(|reference| reference.peel_to_commit())
            .map_err(|error| {
                GitAnalysisError::Failed(format!("no readable HEAD commit: {}", error.message()))
            })?;
        let mut options = StatusOptions::new();
        options.include_untracked(false).include_ignored(false);
        let working_tree_dirty = repo
            .statuses(Some(&mut options))
            .map_err(failed)?
            .iter()
            .next()
            .is_some();
        let author_time = head.author().when().seconds();
        Ok(HeadInfo {
            id: CommitId(head.id().to_string()),
            author_time,
            shallow: repo.is_shallow(),
            working_tree_dirty,
        })
    }

    fn commits(
        &self,
        request: &HistoryRequest,
        cancel: &CancellationToken,
    ) -> Result<CommitBatch, GitAnalysisError> {
        let (repo, prefix) = self.open()?;
        let mut walk = repo.revwalk().map_err(failed)?;
        walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)
            .map_err(failed)?;
        walk.push_head().map_err(failed)?;
        if let Some(known) = &request.stop_at {
            walk.hide(oid_of(known)?).map_err(failed)?;
        }
        let mut batch = CommitBatch {
            commits: Vec::new(),
            examined: 0,
            stop: WalkStop::Exhausted,
        };
        for oid in walk {
            if cancel.is_cancelled() {
                return Err(GitAnalysisError::Cancelled);
            }
            let oid = oid.map_err(failed)?;
            batch.examined += 1;
            if batch.examined > request.max_walk {
                batch.stop = WalkStop::WalkCap;
                break;
            }
            if batch.commits.len() >= request.max_commits {
                batch.stop = WalkStop::CommitLimit;
                break;
            }
            let commit = repo.find_commit(oid).map_err(failed)?;
            let time = commit.author().when().seconds();
            if request.cutoff_time.is_some_and(|cutoff| time < cutoff) {
                batch.stop = WalkStop::AgeLimit;
                break;
            }
            batch
                .commits
                .push(commit_fact(&repo, &commit, &prefix, request)?);
        }
        Ok(batch)
    }

    fn is_ancestor(
        &self,
        ancestor: &CommitId,
        descendant: &CommitId,
    ) -> Result<bool, GitAnalysisError> {
        if ancestor == descendant {
            return Ok(true);
        }
        let (repo, _) = self.open()?;
        repo.graph_descendant_of(oid_of(descendant)?, oid_of(ancestor)?)
            .map_err(failed)
    }

    fn present_at_head(
        &self,
        paths: &[RootRelativePath],
    ) -> Result<BTreeSet<RootRelativePath>, GitAnalysisError> {
        let (repo, prefix) = self.open()?;
        let tree = repo.head().and_then(|h| h.peel_to_tree()).map_err(failed)?;
        Ok(paths
            .iter()
            .filter(|path| tree.get_path(&prefix.join(path.as_path())).is_ok())
            .cloned()
            .collect())
    }
}

fn commit_fact(
    repo: &Repository,
    commit: &git2::Commit<'_>,
    prefix: &Path,
    request: &HistoryRequest,
) -> Result<CommitFact, GitAnalysisError> {
    let author = commit.author();
    let contributor = Contributor {
        id: ContributorId::from_email(author.email().unwrap_or("")),
        name: String::from_utf8_lossy(author.name_bytes()).into_owned(),
    };
    let parent_count = commit.parent_count();
    let changes = if parent_count > 1 {
        Vec::new()
    } else {
        commit_diff::changes(repo, commit, prefix, request.max_line_stat_files)?
    };
    Ok(CommitFact {
        id: CommitId(commit.id().to_string()),
        parent_count,
        author_time: author.when().seconds(),
        contributor,
        changes,
    })
}
