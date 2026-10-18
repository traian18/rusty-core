#![warn(clippy::all)]

//! Agent-facing tools over the repository contracts.
//!
//! Three opt-in toolsets, enabled independently:
//!
//! * `repo_graph.*` needs an `Arc<dyn RepositoryGraph>` (structure, imports, groups, impact).
//! * `repo_git.*` needs an `Arc<dyn GitAnalysis>` (churn, co-change, recent contributors).
//! * `repo_findings.*` needs an `Arc<dyn FindingsAnalysis>` (ranked findings and candidates).
//!
//! A tool whose dependency was not supplied is simply not built, so the factory can fail
//! closed exactly as it does for `code_graph.*`. Every tool returns the same envelope
//! (`schema_version`, snapshot ids, component versions, `partial`, `truncation`, `result`) or
//! a structured error with a stable code.
mod args;
mod context;
mod failure;
mod findings;
mod git;
mod graph;
mod operation;
mod wiki;

use std::sync::Arc;

use harness_findings::FindingsAnalysis;
use harness_git_analysis::GitAnalysis;
use harness_tools::ToolExecutor;
use harness_workspace::repository_intelligence::RepositoryGraph;

pub use operation::SCHEMA_VERSION;

/// The repository knowledge one authorized repository context shares with its sessions.
///
/// Share one value only within one explicitly authorized repository context.
#[derive(Clone, Default)]
pub struct RepositoryIntelligence {
    graph: Option<Arc<dyn RepositoryGraph>>,
    git: Option<Arc<dyn GitAnalysis>>,
    findings: Option<Arc<dyn FindingsAnalysis>>,
}

impl RepositoryIntelligence {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enables the `repo_graph.*` toolset.
    pub fn with_graph(mut self, graph: Arc<dyn RepositoryGraph>) -> Self {
        self.graph = Some(graph);
        self
    }

    /// Enables the `repo_git.*` toolset (and co-change hints in `repo_graph.impact`).
    pub fn with_git(mut self, git: Arc<dyn GitAnalysis>) -> Self {
        self.git = Some(git);
        self
    }

    /// Enables the `repo_findings.*` toolset.
    pub fn with_findings(mut self, findings: Arc<dyn FindingsAnalysis>) -> Self {
        self.findings = Some(findings);
        self
    }

    /// The tool called `name`, or `None` when it is unknown or its toolset is not enabled.
    pub fn executor_for(&self, name: &str) -> Option<Arc<dyn ToolExecutor>> {
        if let Some(graph) = &self.graph {
            let context = graph::GraphContext {
                graph: graph.clone(),
                git: self.git.clone(),
                findings: self.findings.clone(),
            };
            if let Some(tool) = graph::executor_for(name, &context) {
                return Some(tool);
            }
        }
        if let Some(findings) = &self.findings {
            let context = findings::FindingsContext {
                findings: findings.clone(),
            };
            if let Some(tool) = findings::executor_for(name, &context) {
                return Some(tool);
            }
        }
        let git = self.git.as_ref()?;
        git::executor_for(
            name,
            &git::GitContext {
                git: git.clone(),
                graph: self.graph.clone(),
            },
        )
    }

    /// Every tool this configuration provides, in a stable order.
    pub fn executors(&self) -> Vec<Arc<dyn ToolExecutor>> {
        graph::NAMES
            .iter()
            .chain(&git::NAMES)
            .chain(&findings::NAMES)
            .filter_map(|name| self.executor_for(name))
            .collect()
    }
}

/// True for any name in the `repo_graph.*` or `repo_git.*` families, enabled or not.
pub fn is_repository_tool(name: &str) -> bool {
    graph::NAMES.contains(&name) || git::NAMES.contains(&name) || findings::NAMES.contains(&name)
}
