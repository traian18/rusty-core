//! Edges aggregated between the groups of one kind: the package/module overview.
use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use super::{
    model::{GroupId, GroupKind},
    projection::Truncation,
    queries::{GroupQueries, GroupQueryError, GroupSummary},
};
use crate::{
    import_graph::{Direction, ImportFilter, ImportKind},
    repository_intelligence::{RootRelativePath, SnapshotId},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OverviewQuery {
    pub snapshot: SnapshotId,
    pub kind: GroupKind,
    pub node_limit: usize,
    pub edge_limit: usize,
    pub filter: ImportFilter,
}

/// All resolved imports from files of one group to files of another.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OverviewEdge {
    pub from: GroupId,
    pub to: GroupId,
    pub runtime: usize,
    pub type_only: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GroupOverview {
    pub snapshot: SnapshotId,
    pub kind: GroupKind,
    pub nodes: Vec<GroupSummary>,
    pub edges: Vec<OverviewEdge>,
    pub truncation: Truncation,
}

impl GroupQueries<'_> {
    /// Groups of `query.kind` (by id) and the import counts between them; self-edges are omitted.
    pub fn overview(&self, query: &OverviewQuery) -> Result<GroupOverview, GroupQueryError> {
        self.ensure_configured()?;
        let limits = &self.groups.limits;
        self.check_bound(query.node_limit, limits.max_projection_nodes)?;
        self.check_bound(query.edge_limit, limits.max_projection_edges)?;
        let all = self
            .groups
            .groups
            .iter()
            .filter(|group| group.kind == query.kind)
            .collect::<Vec<_>>();
        let mut truncation = Truncation::None;
        let included = if all.len() > query.node_limit {
            truncation = Truncation::NodeLimit;
            &all[..query.node_limit]
        } else {
            &all[..]
        };
        // A file belongs to at most one group per kind except import cycles; the first one wins.
        let mut owner_of: HashMap<&RootRelativePath, &GroupId> = HashMap::new();
        for group in included {
            for member in &group.members {
                owner_of.entry(member).or_insert(&group.id);
            }
        }
        let mut counts: BTreeMap<(&GroupId, &GroupId), (usize, usize)> = BTreeMap::new();
        for edge in &self.imports.edges {
            let Some(target) = edge.resolution.target_file() else {
                continue;
            };
            if !query.filter.accepts(edge, Direction::Dependencies) {
                continue;
            }
            let from = owner_of.get(&edge.fact.importing_file);
            let to = owner_of.get(target);
            if let (Some(&from), Some(&to)) = (from, to) {
                if from != to {
                    let entry = counts.entry((from, to)).or_default();
                    match edge.fact.kind {
                        ImportKind::Runtime => entry.0 += 1,
                        ImportKind::TypeOnly => entry.1 += 1,
                    }
                }
            }
        }
        let mut edges = counts
            .into_iter()
            .map(|((from, to), (runtime, type_only))| OverviewEdge {
                from: from.clone(),
                to: to.clone(),
                runtime,
                type_only,
            })
            .collect::<Vec<_>>();
        if edges.len() > query.edge_limit {
            edges.truncate(query.edge_limit);
            truncation = Truncation::EdgeLimit;
        }
        Ok(GroupOverview {
            snapshot: self.snapshot.clone(),
            kind: query.kind,
            nodes: included.iter().map(|g| GroupSummary::from(*g)).collect(),
            edges,
            truncation,
        })
    }

    pub(super) fn check_bound(&self, value: usize, max: usize) -> Result<(), GroupQueryError> {
        if value == 0 || value > max {
            Err(GroupQueryError::InvalidLimit { max })
        } else {
            Ok(())
        }
    }
}
