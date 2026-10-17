//! The `repo_findings.*` toolset over `Arc<dyn FindingsAnalysis>`.
mod analyze;
mod read;

use std::sync::Arc;

use harness_findings::{FindingSet, FindingSetId, FindingsAnalysis};
use harness_tools::ToolExecutor;

use crate::{
    failure::Failure,
    operation::{OperationTool, Success},
};

pub(crate) const NAMES: [&str; 5] = [
    "repo_findings.analyze",
    "repo_findings.summary",
    "repo_findings.list",
    "repo_findings.get",
    "repo_findings.explain",
];

#[derive(Clone)]
pub(crate) struct FindingsContext {
    pub findings: Arc<dyn FindingsAnalysis>,
}

impl FindingsContext {
    /// The named finding set, or the current one.
    pub fn set(&self, id: &Option<String>) -> Result<Arc<FindingSet>, Failure> {
        match id {
            Some(id) => Ok(self.findings.get(&FindingSetId(id.clone()))?),
            None => self.findings.current().ok_or_else(|| {
                Failure::new(
                    "snapshot_not_found",
                    "no findings have been computed yet; call repo_findings.analyze first",
                )
            }),
        }
    }

    /// A success carrying the contributing snapshot ids and rule versions.
    pub fn success(&self, set: &FindingSet, result: serde_json::Value) -> Success {
        let truncated = !set.summary.truncated_rules.is_empty();
        Success {
            snapshot: Some(set.repository_snapshot.0.clone()),
            git_snapshot: set.git_snapshot.as_ref().map(|id| id.0.clone()),
            components: set
                .rules
                .iter()
                .map(|rule| (format!("finding-rule:{}", rule.id), rule.version))
                .collect(),
            // Skipped or truncated rules mean the set is not the full picture.
            partial: truncated || !set.summary.skipped_rules.is_empty(),
            truncation: truncated.then_some("rule_limit"),
            result,
        }
    }
}

pub(crate) fn executor_for(name: &str, context: &FindingsContext) -> Option<Arc<dyn ToolExecutor>> {
    let context = context.clone();
    Some(match name {
        "repo_findings.analyze" => OperationTool::executor(analyze::Analyze(context)),
        "repo_findings.summary" => OperationTool::executor(read::Summary(context)),
        "repo_findings.list" => OperationTool::executor(read::List(context)),
        "repo_findings.get" => OperationTool::executor(read::Get(context)),
        "repo_findings.explain" => OperationTool::executor(read::Explain(context)),
        _ => return None,
    })
}
