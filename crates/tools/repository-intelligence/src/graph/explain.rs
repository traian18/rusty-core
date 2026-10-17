use std::collections::{BTreeMap, HashMap, VecDeque};

use async_trait::async_trait;
use harness_tools::CancellationToken;
use harness_workspace::{
    dependency_groups::{GraphDirection, GraphEdge, GraphProjection, GraphQuery, Truncation},
    import_graph::{ImportEdge, ImportFilter},
    repository_intelligence::{RepositorySnapshot, RootRelativePath},
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{overview::truncation_name, GraphContext};
use crate::{
    args,
    failure::Failure,
    operation::{Operation, Success},
};

const DEFAULT_DEPTH: usize = 5;
const MAX_DEPTH: usize = 8;
const SEARCH_NODES: usize = 5_000;
const SEARCH_EDGES: usize = 20_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    snapshot: Option<String>,
    from: String,
    to: String,
    max_depth: Option<usize>,
}

/// `repo_graph.explain`: how two files relate through imports and groups.
pub(super) struct Explain(pub GraphContext);

#[async_trait]
impl Operation for Explain {
    fn name(&self) -> &'static str {
        "repo_graph.explain"
    }

    fn description(&self) -> &'static str {
        "Explain the relationship between two files: direct import sites in each direction with rule and evidence, shared groups, and the shortest dependency path within a depth bound. States plainly when no relationship was found within the bounds."
    }

    fn schema(&self) -> Value {
        json!({"type":"object","required":["from","to"],"properties":{
            "snapshot":{"type":"string"},
            "from":{"type":"string","description":"Workspace-relative file"},
            "to":{"type":"string","description":"Workspace-relative file"},
            "max_depth":{"type":"integer","minimum":1,"maximum":MAX_DEPTH}},
            "additionalProperties":false})
    }

    async fn run(&self, arguments: Value, _: &CancellationToken) -> Result<Success, Failure> {
        let args: Arguments = args::parse(arguments)?;
        let depth = args.max_depth.unwrap_or(DEFAULT_DEPTH);
        if depth == 0 || depth > MAX_DEPTH {
            return Err(Failure::new(
                "limit_exceeded",
                format!("max_depth must be between 1 and {MAX_DEPTH}"),
            ));
        }
        let snapshot = self.0.snapshot(&args.snapshot)?;
        let (from, to) = (args::file(&args.from)?, args::file(&args.to)?);
        let forward = self.search(&snapshot, &from, depth)?;
        let backward = self.search(&snapshot, &to, depth)?;
        let from_to_to = shortest_path(&forward, &from, &to);
        let to_to_from = shortest_path(&backward, &to, &from);
        let truncated = [forward.truncation, backward.truncation]
            .into_iter()
            .find(|t| *t != Truncation::None);
        let found = from_to_to.is_some() || to_to_from.is_some();
        let result = json!({
            "from": from,
            "to": to,
            "from_depends_on_to": from_to_to.as_ref().map(|p| path_json(&snapshot, p)),
            "to_depends_on_from": to_to_from.as_ref().map(|p| path_json(&snapshot, p)),
            "direct": {
                "from_imports_to": direct_edges(&snapshot, &from, &to),
                "to_imports_from": direct_edges(&snapshot, &to, &from),
            },
            "shared_groups": self.shared_groups(&snapshot, &from, &to),
            "search": {"max_depth": depth, "from_truncation": truncation_name(forward.truncation), "to_truncation": truncation_name(backward.truncation)},
            "conclusion": if found { "dependency_path_found" } else { "no_dependency_path_within_bounds" },
        });
        let truncation = if found {
            None
        } else {
            truncated.and_then(truncation_name)
        };
        Ok(self.0.success(&snapshot, truncation, result))
    }
}

impl Explain {
    fn search(
        &self,
        snapshot: &RepositorySnapshot,
        root: &RootRelativePath,
        depth: usize,
    ) -> Result<GraphProjection, Failure> {
        Ok(self.0.graph.graph(&GraphQuery {
            snapshot: snapshot.id.clone(),
            roots: vec![root.clone()],
            direction: GraphDirection::Dependencies,
            depth,
            node_limit: SEARCH_NODES,
            edge_limit: SEARCH_EDGES,
            filter: ImportFilter::default(),
        })?)
    }

    /// Groups both files belong to; empty (not an error) when groups are not configured.
    fn shared_groups(
        &self,
        snapshot: &RepositorySnapshot,
        from: &RootRelativePath,
        to: &RootRelativePath,
    ) -> Value {
        let groups_of = |file| {
            self.0
                .graph
                .groups_of(&snapshot.id, file)
                .unwrap_or_default()
        };
        let theirs = groups_of(to)
            .into_iter()
            .map(|a| a.group.id)
            .collect::<Vec<_>>();
        let shared = groups_of(from)
            .into_iter()
            .filter(|a| theirs.contains(&a.group.id))
            .map(|a| a.group)
            .collect::<Vec<_>>();
        json!(shared)
    }
}

/// Breadth-first shortest path over the projection's edges, in projection order.
fn shortest_path(
    projection: &GraphProjection,
    start: &RootRelativePath,
    goal: &RootRelativePath,
) -> Option<Vec<GraphEdge>> {
    if start == goal {
        return Some(Vec::new());
    }
    let mut outgoing: BTreeMap<&RootRelativePath, Vec<&GraphEdge>> = BTreeMap::new();
    for edge in &projection.edges {
        outgoing.entry(&edge.from).or_default().push(edge);
    }
    let mut parent: HashMap<&RootRelativePath, &GraphEdge> = HashMap::new();
    let mut queue = VecDeque::from([start]);
    while let Some(file) = queue.pop_front() {
        for edge in outgoing.get(file).into_iter().flatten() {
            if edge.to == *start || parent.contains_key(&edge.to) {
                continue;
            }
            parent.insert(&edge.to, edge);
            if edge.to == *goal {
                let mut path = vec![(*edge).clone()];
                let mut current = &edge.from;
                while let Some(previous) = parent.get(current) {
                    path.push((*previous).clone());
                    current = &previous.from;
                }
                path.reverse();
                return Some(path);
            }
            queue.push_back(&edge.to);
        }
    }
    None
}

fn path_json(snapshot: &RepositorySnapshot, path: &[GraphEdge]) -> Value {
    let imports = path
        .iter()
        .filter_map(|hop| {
            snapshot
                .imports
                .edges
                .iter()
                .find(|e| e.fact.id == hop.fact)
        })
        .collect::<Vec<_>>();
    let mut files = path
        .first()
        .map(|hop| vec![hop.from.clone()])
        .unwrap_or_default();
    files.extend(path.iter().map(|hop| hop.to.clone()));
    json!({"length": path.len(), "files": files, "imports": imports})
}

/// Import sites in `importer` whose resolved target is `target`.
fn direct_edges<'a>(
    snapshot: &'a RepositorySnapshot,
    importer: &RootRelativePath,
    target: &RootRelativePath,
) -> Vec<&'a ImportEdge> {
    snapshot
        .imports
        .edges
        .iter()
        .filter(|edge| {
            edge.fact.importing_file == *importer && edge.resolution.target_file() == Some(target)
        })
        .collect()
}
