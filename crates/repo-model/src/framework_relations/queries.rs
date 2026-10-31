//! Paged, snapshot-bound queries over the relations of one snapshot.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::model::{FrameworkIndex, FrameworkRelation, RelationKind};
use crate::repository_intelligence::{PackageId, RootRelativePath, SnapshotId};

/// Position in a paged result, bound to one snapshot and one query shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameworkCursor {
    pub snapshot: SnapshotId,
    pub fingerprint: String,
    pub offset: usize,
}

/// Narrows a query. `None` fields accept everything.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameworkFilter {
    pub framework: Option<String>,
    pub kind: Option<RelationKind>,
    /// Relations that start or end in this file.
    pub file: Option<RootRelativePath>,
    pub package: Option<PackageId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameworkQuery {
    pub snapshot: SnapshotId,
    pub filter: FrameworkFilter,
    pub cursor: Option<FrameworkCursor>,
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameworkPage {
    pub snapshot: SnapshotId,
    pub relations: Vec<FrameworkRelation>,
    pub next_cursor: Option<FrameworkCursor>,
    /// True when a limit left some relations out of the snapshot.
    pub partial: bool,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum FrameworkQueryError {
    #[error("framework analysis is not configured for this repository")]
    NotConfigured,
    #[error("snapshot not found: {0}")]
    SnapshotNotFound(String),
    #[error("limit must be between 1 and {max}")]
    InvalidLimit { max: usize },
    #[error("cursor belongs to a different snapshot")]
    ForeignCursor,
    #[error("cursor does not match this query")]
    InvalidCursor,
}

impl FrameworkFilter {
    fn accepts(&self, relation: &FrameworkRelation) -> bool {
        self.framework
            .as_ref()
            .is_none_or(|f| *f == relation.framework)
            && self.kind.is_none_or(|k| k == relation.kind)
            && self
                .file
                .as_ref()
                .is_none_or(|f| relation.from == *f || relation.to.as_ref() == Some(f))
            && self
                .package
                .as_ref()
                .is_none_or(|p| relation.package.as_ref() == Some(p))
    }
}

impl FrameworkIndex {
    /// Runs one paged query; `snapshot` names the snapshot that owns this index.
    pub fn query(
        &self,
        snapshot: &SnapshotId,
        query: &FrameworkQuery,
    ) -> Result<FrameworkPage, FrameworkQueryError> {
        if !self.configured {
            return Err(FrameworkQueryError::NotConfigured);
        }
        if query.limit == 0 || query.limit > self.limits.max_page_limit {
            return Err(FrameworkQueryError::InvalidLimit {
                max: self.limits.max_page_limit,
            });
        }
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&query.filter).unwrap_or_default())
        );
        let offset = match &query.cursor {
            None => 0,
            Some(cursor) if cursor.snapshot != *snapshot => {
                return Err(FrameworkQueryError::ForeignCursor)
            }
            Some(cursor) if cursor.fingerprint != fingerprint => {
                return Err(FrameworkQueryError::InvalidCursor)
            }
            Some(cursor) => cursor.offset,
        };
        let mut selected: Vec<FrameworkRelation> = self
            .relations
            .iter()
            .filter(|relation| query.filter.accepts(relation))
            .skip(offset)
            .take(query.limit + 1)
            .cloned()
            .collect();
        let next_cursor = (selected.len() > query.limit).then(|| {
            selected.truncate(query.limit);
            FrameworkCursor {
                snapshot: snapshot.clone(),
                fingerprint,
                offset: offset + query.limit,
            }
        });
        Ok(FrameworkPage {
            snapshot: snapshot.clone(),
            relations: selected,
            next_cursor,
            partial: self.partial,
        })
    }
}
