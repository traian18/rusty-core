//! The `repo_graph.*` toolset over `Arc<dyn RepositoryGraph>`.
mod capabilities;
mod edges;
mod explain;
mod frameworks;
mod groups;
mod impact;
mod overview;
mod refresh;

use std::sync::Arc;

use harness_git_analysis::GitAnalysis;
use harness_tools::ToolExecutor;
use harness_workspace::repository_intelligence::{RepositoryGraph, RepositorySnapshot, SnapshotId};

use crate::{
    failure::Failure,
    operation::{OperationTool, Success},
};

pub(crate) const NAMES: [&str; 9] = [
    "repo_graph.refresh",
    "repo_graph.capabilities",
    "repo_graph.overview",
    "repo_graph.dependencies",
    "repo_graph.dependents",
    "repo_graph.groups",
    "repo_graph.explain",
    "repo_graph.impact",
    "repo_graph.framework_relations",
];

/// What every graph operation shares: the graph, and Git for the optional co-change hints.
#[derive(Clone)]
pub(crate) struct GraphContext {
    pub graph: Arc<dyn RepositoryGraph>,
    pub git: Option<Arc<dyn GitAnalysis>>,
}

impl GraphContext {
    /// The named snapshot, or the current one; fails with `snapshot_not_found` when neither exists.
    pub fn snapshot(&self, id: &Option<String>) -> Result<Arc<RepositorySnapshot>, Failure> {
        match id {
            Some(id) => Ok(self.graph.get(&SnapshotId(id.clone()))?),
            None => self.graph.current().ok_or_else(|| {
                Failure::new(
                    "snapshot_not_found",
                    "no snapshot has been published yet; call repo_graph.refresh first",
                )
            }),
        }
    }

    /// A success carrying the snapshot id, component versions, and partial flag.
    pub fn success(
        &self,
        snapshot: &RepositorySnapshot,
        truncation: Option<&'static str>,
        result: serde_json::Value,
    ) -> Success {
        let components = snapshot
            .imports
            .components
            .iter()
            .chain(&snapshot.groups.rules)
            .chain(&snapshot.frameworks.components())
            .map(|component| (component.id.clone(), component.version))
            .collect();
        Success {
            snapshot: Some(snapshot.id.0.clone()),
            git_snapshot: None,
            components,
            partial: snapshot.imports.partial,
            truncation,
            result,
        }
    }
}

/// The tool for `name`, or `None` when this toolset does not provide it.
pub(crate) fn executor_for(name: &str, context: &GraphContext) -> Option<Arc<dyn ToolExecutor>> {
    let context = context.clone();
    Some(match name {
        "repo_graph.refresh" => OperationTool::executor(refresh::Refresh(context)),
        "repo_graph.capabilities" => OperationTool::executor(capabilities::Capabilities(context)),
        "repo_graph.overview" => OperationTool::executor(overview::Overview(context)),
        "repo_graph.dependencies" => OperationTool::executor(edges::Edges::dependencies(context)),
        "repo_graph.dependents" => OperationTool::executor(edges::Edges::dependents(context)),
        "repo_graph.groups" => OperationTool::executor(groups::Groups(context)),
        "repo_graph.explain" => OperationTool::executor(explain::Explain(context)),
        "repo_graph.impact" => OperationTool::executor(impact::Impact(context)),
        "repo_graph.framework_relations" => {
            OperationTool::executor(frameworks::FrameworkRelations(context))
        }
        _ => return None,
    })
}
