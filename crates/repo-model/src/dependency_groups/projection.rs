//! Bounded, cycle-safe file subgraph around chosen roots.
use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use super::queries::{GroupQueries, GroupQueryError};
use crate::{
    import_graph::{ImportEdge, ImportFactId, ImportFilter, ImportKind},
    repository_intelligence::{PackageId, RootRelativePath, SnapshotId},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphDirection {
    Dependencies,
    Dependents,
    Both,
}

/// Why a result is smaller than the full reachable subgraph.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Truncation {
    None,
    /// More edges exist beyond the requested depth.
    DepthLimit,
    NodeLimit,
    EdgeLimit,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphQuery {
    pub snapshot: SnapshotId,
    pub roots: Vec<RootRelativePath>,
    pub direction: GraphDirection,
    pub depth: usize,
    pub node_limit: usize,
    pub edge_limit: usize,
    pub filter: ImportFilter,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    pub file: RootRelativePath,
    pub package: Option<PackageId>,
    /// Edges from the nearest root.
    pub depth: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from: RootRelativePath,
    pub to: RootRelativePath,
    pub fact: ImportFactId,
    pub kind: ImportKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphProjection {
    pub snapshot: SnapshotId,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub truncation: Truncation,
}

impl GroupQueries<'_> {
    /// Breadth-first projection over resolved internal imports; deterministic and bounded.
    pub fn graph(&self, query: &GraphQuery) -> Result<GraphProjection, GroupQueryError> {
        self.ensure_configured()?;
        let roots = self.validated_roots(query)?;
        let mut walk = Walk::new(self, query);
        for root in roots {
            walk.add_root(root);
        }
        let mut frontier = walk.nodes.keys().cloned().collect::<Vec<_>>();
        for level in 0..=query.depth {
            if level == query.depth {
                if walk.truncation == Truncation::None && walk.has_unseen_edges(&frontier) {
                    walk.truncation = Truncation::DepthLimit;
                }
                break;
            }
            let Some(next) = walk.expand(&frontier, level + 1) else {
                break;
            };
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        Ok(walk.finish(self))
    }

    fn validated_roots(
        &self,
        query: &GraphQuery,
    ) -> Result<BTreeSet<RootRelativePath>, GroupQueryError> {
        let limits = &self.groups.limits;
        self.check_bound(query.node_limit, limits.max_projection_nodes)?;
        self.check_bound(query.edge_limit, limits.max_projection_edges)?;
        if query.depth > limits.max_projection_depth {
            return Err(GroupQueryError::InvalidQuery(format!(
                "depth must be at most {}",
                limits.max_projection_depth
            )));
        }
        if query.roots.is_empty() || query.roots.len() > limits.max_projection_roots {
            return Err(GroupQueryError::InvalidQuery(format!(
                "between 1 and {} roots are required",
                limits.max_projection_roots
            )));
        }
        for root in &query.roots {
            if !self.imports.has_file(root) {
                return Err(GroupQueryError::UnknownFile(root.clone()));
            }
        }
        Ok(query.roots.iter().cloned().collect())
    }
}

struct Walk<'q, 'a> {
    queries: &'q GroupQueries<'a>,
    query: &'q GraphQuery,
    nodes: BTreeMap<RootRelativePath, usize>,
    edges: Vec<GraphEdge>,
    seen_edges: HashSet<ImportFactId>,
    truncation: Truncation,
}

impl<'q, 'a> Walk<'q, 'a> {
    fn new(queries: &'q GroupQueries<'a>, query: &'q GraphQuery) -> Self {
        Self {
            queries,
            query,
            nodes: BTreeMap::new(),
            edges: Vec::new(),
            seen_edges: HashSet::new(),
            truncation: Truncation::None,
        }
    }

    fn add_root(&mut self, root: RootRelativePath) {
        if self.nodes.len() >= self.query.node_limit {
            self.truncation = Truncation::NodeLimit;
        } else {
            self.nodes.insert(root, 0);
        }
    }

    /// Internal edges touching `file` in the requested direction, with the file on the far side.
    fn neighbours<'e>(&self, file: &RootRelativePath) -> Vec<(&'e ImportEdge, RootRelativePath)>
    where
        'a: 'e,
    {
        use crate::import_graph::Direction;
        let imports = self.queries.imports;
        let wants_out = self.query.direction != GraphDirection::Dependents;
        let wants_in = self.query.direction != GraphDirection::Dependencies;
        let mut found = Vec::new();
        if wants_out {
            for edge in imports.outgoing(file) {
                if let Some(target) = edge.resolution.target_file() {
                    if self.query.filter.accepts(edge, Direction::Dependencies) {
                        found.push((edge, target.clone()));
                    }
                }
            }
        }
        if wants_in {
            for edge in imports.incoming(file) {
                if self.query.filter.accepts(edge, Direction::Dependents) {
                    found.push((edge, edge.fact.importing_file.clone()));
                }
            }
        }
        found
    }

    fn has_unseen_edges(&self, frontier: &[RootRelativePath]) -> bool {
        frontier.iter().any(|file| {
            self.neighbours(file)
                .iter()
                .any(|(edge, _)| !self.seen_edges.contains(&edge.fact.id))
        })
    }

    /// Visits `frontier`; returns the next frontier, or `None` once the edge limit stops the walk.
    fn expand(
        &mut self,
        frontier: &[RootRelativePath],
        depth: usize,
    ) -> Option<Vec<RootRelativePath>> {
        let mut next = BTreeSet::new();
        for file in frontier {
            for (edge, neighbour) in self.neighbours(file) {
                if self.seen_edges.contains(&edge.fact.id) {
                    continue;
                }
                if !self.nodes.contains_key(&neighbour) {
                    if self.nodes.len() >= self.query.node_limit {
                        self.truncation = Truncation::NodeLimit;
                        continue;
                    }
                    self.nodes.insert(neighbour.clone(), depth);
                    next.insert(neighbour.clone());
                }
                if self.edges.len() >= self.query.edge_limit {
                    self.truncation = Truncation::EdgeLimit;
                    return None;
                }
                self.seen_edges.insert(edge.fact.id.clone());
                let target = edge.resolution.target_file()?.clone();
                self.edges.push(GraphEdge {
                    from: edge.fact.importing_file.clone(),
                    to: target,
                    fact: edge.fact.id.clone(),
                    kind: edge.fact.kind,
                });
            }
        }
        Some(next.into_iter().collect())
    }

    fn finish(self, queries: &GroupQueries<'_>) -> GraphProjection {
        let packages = queries
            .imports
            .files
            .iter()
            .map(|file| (&file.path, &file.package))
            .collect::<BTreeMap<_, _>>();
        GraphProjection {
            snapshot: queries.snapshot.clone(),
            nodes: self
                .nodes
                .into_iter()
                .map(|(file, depth)| GraphNode {
                    package: packages.get(&file).and_then(|p| (*p).clone()),
                    file,
                    depth,
                })
                .collect(),
            edges: self.edges,
            truncation: self.truncation,
        }
    }
}
