use async_trait::async_trait;
use harness_git_analysis::RepositoryCorrelation;
use harness_tools::CancellationToken;
use serde::Deserialize;
use serde_json::{json, Value};

use super::GitContext;
use crate::{
    args,
    failure::Failure,
    operation::{Operation, Success},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArguments {}

/// `repo_git.refresh`: reads the history window and publishes a Git snapshot.
pub(super) struct Refresh(pub GitContext);

#[async_trait]
impl Operation for Refresh {
    fn name(&self) -> &'static str {
        "repo_git.refresh"
    }

    fn description(&self) -> &'static str {
        "Read local Git history within the configured window and publish an immutable snapshot of file churn, co-change and recent contributors, joined to the current repository snapshot. Historical correlation only, not causation or ownership."
    }

    fn schema(&self) -> Value {
        json!({"type":"object","additionalProperties":false})
    }

    async fn run(&self, arguments: Value, cancel: &CancellationToken) -> Result<Success, Failure> {
        args::parse::<NoArguments>(arguments)?;
        let correlation = self
            .0
            .graph
            .as_ref()
            .and_then(|graph| graph.current())
            .map(|snapshot| RepositoryCorrelation::from_snapshot(&snapshot))
            .unwrap_or_default();
        let snapshot = self.0.git.refresh(correlation, cancel.clone()).await?;
        let result = json!({
            "head": snapshot.head,
            "repository_snapshot": snapshot.repository_snapshot,
            "files": snapshot.index.churn.len(),
            "co_change_relations": snapshot.index.co_changes.len(),
            "coverage": snapshot.coverage,
            "config": snapshot.config,
        });
        Ok(self.0.success(&snapshot, result))
    }
}
