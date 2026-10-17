//! The `repo_git.*` toolset over `Arc<dyn GitAnalysis>`.
mod queries;
mod refresh;

use std::sync::Arc;

use harness_git_analysis::{GitAnalysis, GitCoverage, GitSnapshot, GitSnapshotId, WalkStop};
use harness_tools::ToolExecutor;
use harness_workspace::repository_intelligence::RepositoryGraph;

use crate::{
    failure::Failure,
    operation::{OperationTool, Success},
};

pub(crate) const NAMES: [&str; 5] = [
    "repo_git.refresh",
    "repo_git.churn",
    "repo_git.co_changed",
    "repo_git.contributors",
    "repo_git.coverage",
];

/// Version reported for the Git analysis component.
const ANALYSIS_VERSION: u32 = 1;

/// The analysis, plus the graph whose current snapshot refresh correlates with.
#[derive(Clone)]
pub(crate) struct GitContext {
    pub git: Arc<dyn GitAnalysis>,
    pub graph: Option<Arc<dyn RepositoryGraph>>,
}

impl GitContext {
    /// The named Git snapshot, or the current one.
    pub fn snapshot(&self, id: &Option<String>) -> Result<Arc<GitSnapshot>, Failure> {
        match id {
            Some(id) => Ok(self.git.get(&GitSnapshotId(id.clone()))?),
            None => self.git.current().ok_or_else(|| {
                Failure::new(
                    "snapshot_not_found",
                    "no Git snapshot has been published yet; call repo_git.refresh first",
                )
            }),
        }
    }

    pub fn success(&self, snapshot: &GitSnapshot, result: serde_json::Value) -> Success {
        let coverage = &snapshot.coverage;
        Success {
            snapshot: None,
            git_snapshot: Some(snapshot.id.0.clone()),
            components: vec![("git-analysis".to_owned(), ANALYSIS_VERSION)],
            partial: coverage.shallow || coverage.walk_stop == WalkStop::WalkCap,
            truncation: truncation_of(coverage),
            result,
        }
    }
}

fn truncation_of(coverage: &GitCoverage) -> Option<&'static str> {
    match coverage.walk_stop {
        WalkStop::Exhausted => None,
        WalkStop::CommitLimit => Some("commit_limit"),
        WalkStop::AgeLimit => Some("age_limit"),
        WalkStop::WalkCap => Some("walk_cap"),
    }
}

pub(crate) fn executor_for(name: &str, context: &GitContext) -> Option<Arc<dyn ToolExecutor>> {
    let context = context.clone();
    Some(match name {
        "repo_git.refresh" => OperationTool::executor(refresh::Refresh(context)),
        "repo_git.churn" => OperationTool::executor(queries::Churn(context)),
        "repo_git.co_changed" => OperationTool::executor(queries::CoChanged(context)),
        "repo_git.contributors" => OperationTool::executor(queries::Contributors(context)),
        "repo_git.coverage" => OperationTool::executor(queries::Coverage(context)),
        _ => return None,
    })
}
