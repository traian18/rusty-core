//! Paged, snapshot-bound queries over a [`GitSnapshot`].
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use harness_workspace::repository_intelligence::RootRelativePath;

use crate::{
    error::GitQueryError,
    snapshot::{CoChange, ContributorShare, FileChurn, GitSnapshot, GitSnapshotId, INTERPRETATION},
};

const CONTRIBUTOR_DEFINITION: &str = "Recent contributors: each author's share of commits and \
changed lines in the analysed window, grouped by a hash of the author email. This is not code ownership.";

/// Position in a paged result, bound to one snapshot and one query shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GitCursor {
    pub snapshot: GitSnapshotId,
    pub fingerprint: String,
    pub offset: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChurnScope {
    Repository,
    File(RootRelativePath),
    /// Files under a directory.
    Prefix(RootRelativePath),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChurnSort {
    Commits,
    LinesChanged,
    LastChanged,
    Path,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChurnQuery {
    pub snapshot: GitSnapshotId,
    pub scope: ChurnScope,
    pub sort: ChurnSort,
    pub cursor: Option<GitCursor>,
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChurnPage {
    pub snapshot: GitSnapshotId,
    pub entries: Vec<FileChurn>,
    pub next_cursor: Option<GitCursor>,
    pub interpretation: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CoChangeQuery {
    pub snapshot: GitSnapshotId,
    pub file: RootRelativePath,
    /// Raise the snapshot's minimum support; lowering it is not possible.
    pub min_support: Option<u32>,
    pub min_confidence_per_mille: Option<u32>,
    pub cursor: Option<GitCursor>,
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CoChangePage {
    pub snapshot: GitSnapshotId,
    pub file: RootRelativePath,
    /// Strongest partner first.
    pub entries: Vec<CoChange>,
    pub next_cursor: Option<GitCursor>,
    pub interpretation: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContributorQuery {
    pub snapshot: GitSnapshotId,
    pub file: RootRelativePath,
    pub cursor: Option<GitCursor>,
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContributorPage {
    pub snapshot: GitSnapshotId,
    pub file: RootRelativePath,
    pub definition: String,
    pub entries: Vec<ContributorShare>,
    pub next_cursor: Option<GitCursor>,
    pub interpretation: String,
}

impl GitSnapshot {
    pub fn churn_page(&self, query: &ChurnQuery) -> Result<ChurnPage, GitQueryError> {
        let shape = fingerprint(&(&query.scope, query.sort));
        let offset = self.page_start(query.limit, &query.cursor, &shape)?;
        let mut entries = match &query.scope {
            ChurnScope::File(file) => vec![self.churn_of(file)?.clone()],
            scope => self.scoped_churn(scope),
        };
        sort_churn(&mut entries, query.sort);
        let (entries, next_cursor) = self.slice(entries, offset, query.limit, &shape);
        Ok(ChurnPage {
            snapshot: self.id.clone(),
            entries,
            next_cursor,
            interpretation: INTERPRETATION.to_owned(),
        })
    }

    pub fn co_change_page(&self, query: &CoChangeQuery) -> Result<CoChangePage, GitQueryError> {
        self.churn_of(&query.file)?;
        let min_support = query.min_support.unwrap_or(self.config.min_support);
        let min_confidence = query
            .min_confidence_per_mille
            .unwrap_or(self.config.min_confidence_per_mille);
        if min_support < self.config.min_support
            || min_confidence < self.config.min_confidence_per_mille
        {
            return Err(GitQueryError::InvalidQuery(
                "thresholds cannot be lower than the snapshot's own minimums".into(),
            ));
        }
        let shape = fingerprint(&(&query.file, min_support, min_confidence));
        let offset = self.page_start(query.limit, &query.cursor, &shape)?;
        let start = self
            .index
            .co_changes
            .partition_point(|c| c.file < query.file);
        let partners = self.index.co_changes[start..]
            .iter()
            .take_while(|c| c.file == query.file)
            .filter(|c| c.support >= min_support && c.confidence_per_mille >= min_confidence)
            .cloned()
            .collect::<Vec<_>>();
        let (entries, next_cursor) = self.slice(partners, offset, query.limit, &shape);
        Ok(CoChangePage {
            snapshot: self.id.clone(),
            file: query.file.clone(),
            entries,
            next_cursor,
            interpretation: INTERPRETATION.to_owned(),
        })
    }

    pub fn contributor_page(
        &self,
        query: &ContributorQuery,
    ) -> Result<ContributorPage, GitQueryError> {
        self.churn_of(&query.file)?;
        let shape = fingerprint(&query.file);
        let offset = self.page_start(query.limit, &query.cursor, &shape)?;
        let position = self
            .index
            .contributors
            .binary_search_by(|c| c.file.cmp(&query.file))
            .map_err(|_| GitQueryError::UnknownFile(display(&query.file)))?;
        let all = self.index.contributors[position].entries.clone();
        let (entries, next_cursor) = self.slice(all, offset, query.limit, &shape);
        Ok(ContributorPage {
            snapshot: self.id.clone(),
            file: query.file.clone(),
            definition: CONTRIBUTOR_DEFINITION.to_owned(),
            entries,
            next_cursor,
            interpretation: INTERPRETATION.to_owned(),
        })
    }

    fn churn_of(&self, file: &RootRelativePath) -> Result<&FileChurn, GitQueryError> {
        self.index
            .churn
            .binary_search_by(|c| c.file.cmp(file))
            .map(|position| &self.index.churn[position])
            .map_err(|_| GitQueryError::UnknownFile(display(file)))
    }

    fn scoped_churn(&self, scope: &ChurnScope) -> Vec<FileChurn> {
        self.index
            .churn
            .iter()
            .filter(|c| match scope {
                ChurnScope::Prefix(prefix) => c.file.as_path().starts_with(prefix.as_path()),
                _ => true,
            })
            .cloned()
            .collect()
    }

    /// Validates the limit and cursor and returns where the page starts.
    fn page_start(
        &self,
        limit: usize,
        cursor: &Option<GitCursor>,
        expected: &str,
    ) -> Result<usize, GitQueryError> {
        if limit == 0 || limit > self.config.max_page_limit {
            return Err(GitQueryError::InvalidLimit {
                max: self.config.max_page_limit,
            });
        }
        match cursor {
            None => Ok(0),
            Some(cursor) if cursor.snapshot != self.id => Err(GitQueryError::ForeignCursor),
            Some(cursor) if cursor.fingerprint != expected => Err(GitQueryError::InvalidCursor),
            Some(cursor) => Ok(cursor.offset),
        }
    }

    fn slice<T>(
        &self,
        items: Vec<T>,
        offset: usize,
        limit: usize,
        shape: &str,
    ) -> (Vec<T>, Option<GitCursor>) {
        let more = items.len() > offset + limit;
        let page = items.into_iter().skip(offset).take(limit).collect();
        let next = more.then(|| GitCursor {
            snapshot: self.id.clone(),
            fingerprint: shape.to_owned(),
            offset: offset + limit,
        });
        (page, next)
    }
}

fn sort_churn(entries: &mut [FileChurn], sort: ChurnSort) {
    entries.sort_by(|a, b| {
        let order = match sort {
            ChurnSort::Commits => b.commits.cmp(&a.commits),
            ChurnSort::LinesChanged => {
                (b.lines_added + b.lines_removed).cmp(&(a.lines_added + a.lines_removed))
            }
            ChurnSort::LastChanged => b.last_change.cmp(&a.last_change),
            ChurnSort::Path => std::cmp::Ordering::Equal,
        };
        order.then_with(|| a.file.cmp(&b.file))
    });
}

fn display(path: &RootRelativePath) -> String {
    path.as_path().display().to_string()
}

fn fingerprint<T: Serialize>(shape: &T) -> String {
    let bytes = serde_json::to_vec(shape).unwrap_or_default();
    format!("{:x}", Sha256::digest(&bytes))
}
