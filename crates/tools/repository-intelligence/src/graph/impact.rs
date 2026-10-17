use std::collections::BTreeMap;

use async_trait::async_trait;
use harness_git_analysis::{CoChangeQuery, GitQueryError};
use harness_tools::CancellationToken;
use harness_workspace::{
    dependency_groups::{GraphDirection, GraphQuery},
    import_graph::{ImportFilter, ImportKind},
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{overview::truncation_name, GraphContext};
use crate::{
    args,
    failure::Failure,
    operation::{Operation, Success},
};

const MAX_DEPTH: usize = 5;
const MAX_NODES: usize = 1_000;
const MAX_EDGES: usize = 5_000;
/// Roots for which co-change hints are looked up.
const HINT_ROOTS: usize = 5;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Direction {
    Dependents,
    Dependencies,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    snapshot: Option<String>,
    files: Vec<String>,
    direction: Option<Direction>,
    depth: Option<usize>,
    node_limit: Option<usize>,
    edge_limit: Option<usize>,
    import_kind: Option<ImportKind>,
    #[serde(default)]
    include_co_change: bool,
}

/// `repo_graph.impact`: what is reachable from some files through imports, within bounds.
pub(super) struct Impact(pub GraphContext);

#[async_trait]
impl Operation for Impact {
    fn name(&self) -> &'static str {
        "repo_graph.impact"
    }

    fn description(&self) -> &'static str {
        "Bounded impact of changing files: transitive dependents (or dependencies) through resolved internal imports, by depth and package, with the truncation reason. Import edges only; include_co_change adds a separate, clearly labelled section of historical co-change hints when Git analysis is available."
    }

    fn schema(&self) -> Value {
        json!({"type":"object","required":["files"],"properties":{
            "snapshot":{"type":"string"},
            "files":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":64},
            "direction":{"type":"string","enum":["dependents","dependencies"],"description":"Default dependents"},
            "depth":{"type":"integer","minimum":1,"maximum":MAX_DEPTH},
            "node_limit":{"type":"integer","minimum":1,"maximum":MAX_NODES},
            "edge_limit":{"type":"integer","minimum":1,"maximum":MAX_EDGES},
            "import_kind":{"type":"string","enum":["runtime","type_only"]},
            "include_co_change":{"type":"boolean"}},
            "additionalProperties":false})
    }

    async fn run(&self, arguments: Value, _: &CancellationToken) -> Result<Success, Failure> {
        let args: Arguments = args::parse(arguments)?;
        let depth = args.depth.unwrap_or(3);
        if depth == 0 || depth > MAX_DEPTH {
            return Err(Failure::new(
                "limit_exceeded",
                format!("depth must be between 1 and {MAX_DEPTH}"),
            ));
        }
        if args.include_co_change && self.0.git.is_none() {
            return Err(Failure::not_configured(
                "include_co_change needs the Git analysis toolset",
            ));
        }
        let snapshot = self.0.snapshot(&args.snapshot)?;
        let roots = args
            .files
            .iter()
            .map(|f| args::file(f))
            .collect::<Result<Vec<_>, _>>()?;
        let direction = match args.direction.unwrap_or(Direction::Dependents) {
            Direction::Dependents => GraphDirection::Dependents,
            Direction::Dependencies => GraphDirection::Dependencies,
        };
        let projection = self.0.graph.graph(&GraphQuery {
            snapshot: snapshot.id.clone(),
            roots: roots.clone(),
            direction,
            depth,
            node_limit: args.node_limit.unwrap_or(200),
            edge_limit: args.edge_limit.unwrap_or(1_000),
            filter: ImportFilter {
                kind: args.import_kind,
                ..ImportFilter::default()
            },
        })?;
        let mut by_depth: BTreeMap<usize, usize> = BTreeMap::new();
        let mut by_package: BTreeMap<String, usize> = BTreeMap::new();
        for node in projection.nodes.iter().filter(|n| n.depth > 0) {
            *by_depth.entry(node.depth).or_default() += 1;
            let package = node
                .package
                .as_ref()
                .map_or("(no package)".to_owned(), |p| p.0.clone());
            *by_package.entry(package).or_default() += 1;
        }
        let mut result = json!({
            "roots": roots,
            "direction": args.direction.map_or("dependents", |d| match d { Direction::Dependents => "dependents", Direction::Dependencies => "dependencies" }),
            "affected_files": projection.nodes.iter().filter(|n| n.depth > 0).count(),
            "by_depth": by_depth,
            "by_package": by_package,
            "nodes": projection.nodes,
            "edges": projection.edges,
        });
        let mut success_git = None;
        if args.include_co_change {
            let (hints, git_snapshot) = self.co_change_hints(&roots)?;
            result["co_change_hints"] = hints;
            success_git = Some(git_snapshot);
        }
        let mut success = self
            .0
            .success(&snapshot, truncation_name(projection.truncation), result);
        success.git_snapshot = success_git;
        Ok(success)
    }
}

impl Impact {
    /// Correlated files per root, kept apart from the import results and labelled as such.
    fn co_change_hints(
        &self,
        roots: &[harness_workspace::repository_intelligence::RootRelativePath],
    ) -> Result<(Value, String), Failure> {
        let git = self
            .0
            .git
            .as_ref()
            .ok_or_else(|| Failure::not_configured("Git analysis is not available"))?;
        let current = git.current().ok_or_else(|| {
            Failure::not_configured("no Git snapshot yet; call repo_git.refresh first")
        })?;
        let mut files = Vec::new();
        for root in roots.iter().take(HINT_ROOTS) {
            let page = git.co_changed(&CoChangeQuery {
                snapshot: current.id.clone(),
                file: root.clone(),
                min_support: None,
                min_confidence_per_mille: None,
                cursor: None,
                limit: 10,
            });
            match page {
                Ok(page) => files.push(json!({"file": root, "partners": page.entries})),
                Err(GitQueryError::UnknownFile(_)) => {
                    files.push(json!({"file": root, "partners": []}))
                }
                Err(error) => return Err(error.into()),
            }
        }
        let hints = json!({
            "label": "historical correlation, not a dependency",
            "interpretation": current.coverage.interpretation,
            "files": files,
        });
        Ok((hints, current.id.0.clone()))
    }
}
