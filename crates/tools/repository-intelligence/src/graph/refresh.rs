use async_trait::async_trait;
use harness_tools::CancellationToken;
use serde::Deserialize;
use serde_json::{json, Value};

use super::GraphContext;
use crate::{
    args,
    failure::Failure,
    operation::{Operation, Success},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArguments {}

/// `repo_graph.refresh`: rescans the repository and publishes a new snapshot.
pub(super) struct Refresh(pub GraphContext);

#[async_trait]
impl Operation for Refresh {
    fn name(&self) -> &'static str {
        "repo_graph.refresh"
    }

    fn description(&self) -> &'static str {
        "Rescan the repository and publish an immutable snapshot of packages, imports and groups; returns its id, coverage, counts and limits. Required before other repo_graph tools when no snapshot exists."
    }

    fn schema(&self) -> Value {
        json!({"type":"object","additionalProperties":false})
    }

    async fn run(&self, arguments: Value, cancel: &CancellationToken) -> Result<Success, Failure> {
        args::parse::<NoArguments>(arguments)?;
        let snapshot = self.0.graph.refresh(cancel.clone()).await?;
        let mut counts = std::collections::BTreeMap::new();
        for edge in &snapshot.imports.edges {
            *counts
                .entry(format!("{:?}", edge.resolution.kind()))
                .or_insert(0usize) += 1;
        }
        let result = json!({
            "packages": snapshot.graph.packages.len(),
            "files": snapshot.imports.files.len(),
            "imports": snapshot.imports.edges.len(),
            "imports_by_resolution": counts,
            "diagnostics": snapshot.graph.diagnostics.len() + snapshot.imports.diagnostics.len(),
            "groups": {"configured": snapshot.groups.configured, "count": snapshot.groups.groups.len()},
            "import_limits": snapshot.imports.limits,
        });
        Ok(self.0.success(&snapshot, None, result))
    }
}
