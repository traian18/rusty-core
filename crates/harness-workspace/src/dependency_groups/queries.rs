//! Read-side facade over one snapshot's groups and import index, plus list/members/explain queries.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::model::{Group, GroupBasis, GroupId, GroupIndex, GroupKind};
use crate::{
    import_graph::{ImportCursor, ImportFactId, ImportIndex},
    repository_intelligence::{RootRelativePath, SnapshotId},
};

#[derive(Debug, Error, Eq, PartialEq)]
pub enum GroupQueryError {
    #[error("snapshot not found: {0}")]
    SnapshotNotFound(String),
    #[error("dependency groups were not configured for this repository graph")]
    NotConfigured,
    #[error("limit must be between 1 and {max}")]
    InvalidLimit { max: usize },
    #[error("cursor belongs to a different snapshot")]
    ForeignCursor,
    #[error("cursor does not match this query")]
    InvalidCursor,
    #[error("unknown group: {0:?}")]
    UnknownGroup(GroupId),
    #[error("file is not part of this snapshot's import index: {0:?}")]
    UnknownFile(RootRelativePath),
    #[error("invalid query: {0}")]
    InvalidQuery(String),
}

/// One snapshot's groups and imports, ready to answer queries.
pub struct GroupQueries<'a> {
    pub snapshot: &'a SnapshotId,
    pub groups: &'a GroupIndex,
    pub imports: &'a ImportIndex,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GroupSummary {
    pub id: GroupId,
    pub kind: GroupKind,
    pub label: String,
    pub basis: GroupBasis,
    pub explanation: String,
    pub member_count: usize,
    pub involves_type_only: bool,
}

impl From<&Group> for GroupSummary {
    fn from(group: &Group) -> Self {
        Self {
            id: group.id.clone(),
            kind: group.kind,
            label: group.label.clone(),
            basis: group.basis,
            explanation: group.explanation.clone(),
            member_count: group.members.len(),
            involves_type_only: group.involves_type_only,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GroupListQuery {
    pub snapshot: SnapshotId,
    pub kind: Option<GroupKind>,
    pub cursor: Option<ImportCursor>,
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GroupPage {
    pub snapshot: SnapshotId,
    pub groups: Vec<GroupSummary>,
    pub next_cursor: Option<ImportCursor>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GroupMembersQuery {
    pub snapshot: SnapshotId,
    pub group: GroupId,
    pub cursor: Option<ImportCursor>,
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemberPage {
    pub snapshot: SnapshotId,
    pub group: GroupSummary,
    pub members: Vec<RootRelativePath>,
    pub next_cursor: Option<ImportCursor>,
}

/// Why a file is in a group.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GroupAssignment {
    pub group: GroupSummary,
    pub evidence: Vec<RootRelativePath>,
    pub cycle_sites: Vec<ImportFactId>,
    pub evidence_truncated: bool,
}

impl GroupQueries<'_> {
    pub fn list(&self, query: &GroupListQuery) -> Result<GroupPage, GroupQueryError> {
        self.ensure_configured()?;
        let offset = self.page_start(query.limit, &query.cursor, &fingerprint(&(query.kind,)))?;
        let mut groups = self
            .groups
            .groups
            .iter()
            .filter(|group| query.kind.is_none_or(|kind| group.kind == kind))
            .skip(offset)
            .take(query.limit + 1)
            .map(GroupSummary::from)
            .collect::<Vec<_>>();
        let next_cursor = self.next_cursor(groups.len(), query.limit, offset, &(query.kind,));
        groups.truncate(query.limit);
        Ok(GroupPage {
            snapshot: self.snapshot.clone(),
            groups,
            next_cursor,
        })
    }

    pub fn members(&self, query: &GroupMembersQuery) -> Result<MemberPage, GroupQueryError> {
        self.ensure_configured()?;
        let offset = self.page_start(query.limit, &query.cursor, &fingerprint(&query.group))?;
        let group = self.group(&query.group)?;
        let mut members = group
            .members
            .iter()
            .skip(offset)
            .take(query.limit + 1)
            .cloned()
            .collect::<Vec<_>>();
        let next_cursor = self.next_cursor(members.len(), query.limit, offset, &query.group);
        members.truncate(query.limit);
        Ok(MemberPage {
            snapshot: self.snapshot.clone(),
            group: GroupSummary::from(group),
            members,
            next_cursor,
        })
    }

    /// Every group containing `file`, each with the evidence behind it.
    pub fn assignments(
        &self,
        file: &RootRelativePath,
    ) -> Result<Vec<GroupAssignment>, GroupQueryError> {
        self.ensure_configured()?;
        if !self.imports.has_file(file) {
            return Err(GroupQueryError::UnknownFile(file.clone()));
        }
        Ok(self
            .groups
            .groups_containing(file)
            .map(|group| GroupAssignment {
                group: GroupSummary::from(group),
                evidence: group.evidence.clone(),
                cycle_sites: group.cycle_sites.clone(),
                evidence_truncated: group.evidence_truncated,
            })
            .collect())
    }

    pub(super) fn ensure_configured(&self) -> Result<(), GroupQueryError> {
        if self.groups.configured {
            Ok(())
        } else {
            Err(GroupQueryError::NotConfigured)
        }
    }

    pub(super) fn group(&self, id: &GroupId) -> Result<&Group, GroupQueryError> {
        self.groups
            .group(id)
            .ok_or_else(|| GroupQueryError::UnknownGroup(id.clone()))
    }

    /// Validates the limit and cursor and returns where the page starts.
    pub(super) fn page_start(
        &self,
        limit: usize,
        cursor: &Option<ImportCursor>,
        expected_fingerprint: &str,
    ) -> Result<usize, GroupQueryError> {
        if limit == 0 || limit > self.groups.limits.max_page_limit {
            return Err(GroupQueryError::InvalidLimit {
                max: self.groups.limits.max_page_limit,
            });
        }
        match cursor {
            None => Ok(0),
            Some(cursor) if cursor.snapshot != *self.snapshot => {
                Err(GroupQueryError::ForeignCursor)
            }
            Some(cursor) if cursor.fingerprint != expected_fingerprint => {
                Err(GroupQueryError::InvalidCursor)
            }
            Some(cursor) => Ok(cursor.offset),
        }
    }

    /// A cursor for the next page when the page fetched one extra item.
    pub(super) fn next_cursor<T: Serialize>(
        &self,
        fetched: usize,
        limit: usize,
        offset: usize,
        shape: &T,
    ) -> Option<ImportCursor> {
        (fetched > limit).then(|| ImportCursor {
            snapshot: self.snapshot.clone(),
            fingerprint: fingerprint(shape),
            offset: offset + limit,
        })
    }
}

/// Identifies the query shape a cursor belongs to.
pub(super) fn fingerprint<T: Serialize>(shape: &T) -> String {
    let bytes = serde_json::to_vec(shape).unwrap_or_default();
    format!("{:x}", Sha256::digest(&bytes))
}
