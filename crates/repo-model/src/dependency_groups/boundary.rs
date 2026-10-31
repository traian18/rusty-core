//! Import edges that cross a group's boundary.
use serde::{Deserialize, Serialize};

use super::{
    model::GroupId,
    queries::{fingerprint, GroupQueries, GroupQueryError},
};
use crate::{
    import_graph::{Direction, ImportCursor, ImportEdge, ImportFilter, ImportPage},
    repository_intelligence::SnapshotId,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryDirection {
    /// Members importing files outside the group.
    Outgoing,
    /// Files outside the group importing members.
    Incoming,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BoundaryQuery {
    pub snapshot: SnapshotId,
    pub group: GroupId,
    pub direction: BoundaryDirection,
    pub filter: ImportFilter,
    pub cursor: Option<ImportCursor>,
    pub limit: usize,
}

impl GroupQueries<'_> {
    /// Resolved internal imports crossing the group boundary, ordered by importing file then position.
    pub fn boundary_edges(&self, query: &BoundaryQuery) -> Result<ImportPage, GroupQueryError> {
        self.ensure_configured()?;
        let shape = (&query.group, query.direction, &query.filter);
        let offset = self.page_start(query.limit, &query.cursor, &fingerprint(&shape))?;
        let group = self.group(&query.group)?;
        let mut edges: Vec<ImportEdge> = self
            .imports
            .edges
            .iter()
            .filter(|edge| crosses(group, edge, query.direction))
            .filter(|edge| {
                query
                    .filter
                    .accepts(edge, query.direction.as_import_direction())
            })
            .skip(offset)
            .take(query.limit + 1)
            .cloned()
            .collect();
        let next_cursor = self.next_cursor(edges.len(), query.limit, offset, &shape);
        edges.truncate(query.limit);
        Ok(ImportPage {
            snapshot: self.snapshot.clone(),
            edges,
            next_cursor,
            partial_coverage: self.imports.partial,
        })
    }
}

impl BoundaryDirection {
    fn as_import_direction(self) -> Direction {
        match self {
            Self::Outgoing => Direction::Dependencies,
            Self::Incoming => Direction::Dependents,
        }
    }
}

fn crosses(group: &super::model::Group, edge: &ImportEdge, direction: BoundaryDirection) -> bool {
    let Some(target) = edge.resolution.target_file() else {
        return false;
    };
    let source_inside = group.contains(&edge.fact.importing_file);
    let target_inside = group.contains(target);
    match direction {
        BoundaryDirection::Outgoing => source_inside && !target_inside,
        BoundaryDirection::Incoming => !source_inside && target_inside,
    }
}
