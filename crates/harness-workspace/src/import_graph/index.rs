//! Immutable import index published inside a repository snapshot, and its queries.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::model::{
    ImportEdge, ImportKind, ImportLimits, LanguageProfile, ResolutionKind, Staticness,
};
use crate::repository_intelligence::{
    DiscoveryDiagnostic, PackageId, RootRelativePath, SnapshotId,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndexedFile {
    pub path: RootRelativePath,
    pub package: Option<PackageId>,
    pub profile: LanguageProfile,
    pub content_hash: String,
    pub fact_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndexedConfig {
    pub path: RootRelativePath,
    pub content_hash: String,
}

/// Identity of an adapter or resolver that contributed to the index.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ComponentVersion {
    pub id: String,
    pub version: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportIndex {
    pub files: Vec<IndexedFile>,
    /// Compiler configs that influenced resolution.
    #[serde(default)]
    pub configs: Vec<IndexedConfig>,
    /// Files a resolver could not attach to any root; sorted.
    #[serde(default)]
    pub unattached: Vec<RootRelativePath>,
    /// Sorted by importing file, then source position.
    pub edges: Vec<ImportEdge>,
    /// Positions of internally resolved edges sorted by target file, then importer and position.
    pub dependents_order: Vec<u32>,
    pub diagnostics: Vec<DiscoveryDiagnostic>,
    /// True when limits or unreadable files left some imports out.
    pub partial: bool,
    pub limits: ImportLimits,
    pub components: Vec<ComponentVersion>,
    /// Digest of everything above; folded into the snapshot id.
    pub digest: String,
}

/// Everything an [`ImportIndex`] is built from; `ImportIndex::new` sorts it into canonical order.
pub struct ImportIndexParts {
    pub files: Vec<IndexedFile>,
    pub configs: Vec<IndexedConfig>,
    pub unattached: Vec<RootRelativePath>,
    pub edges: Vec<ImportEdge>,
    pub diagnostics: Vec<DiscoveryDiagnostic>,
    pub partial: bool,
    pub limits: ImportLimits,
    pub components: Vec<ComponentVersion>,
}

impl ImportIndex {
    pub fn new(parts: ImportIndexParts) -> Self {
        let ImportIndexParts {
            mut files,
            mut configs,
            mut unattached,
            mut edges,
            mut diagnostics,
            partial,
            limits,
            mut components,
        } = parts;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        configs.sort_by(|a, b| a.path.cmp(&b.path));
        unattached.sort();
        unattached.dedup();
        edges.sort_by(|a, b| {
            a.fact
                .importing_file
                .cmp(&b.fact.importing_file)
                .then(a.fact.span.cmp(&b.fact.span))
                .then(a.fact.id.cmp(&b.fact.id))
        });
        diagnostics.sort_by(|a, b| a.path.cmp(&b.path).then(a.message.cmp(&b.message)));
        components.sort();
        let mut dependents_order = edges
            .iter()
            .enumerate()
            .filter(|(_, edge)| edge.resolution.target_file().is_some())
            .map(|(position, _)| position as u32)
            .collect::<Vec<_>>();
        dependents_order.sort_by(|&a, &b| {
            let (a, b) = (&edges[a as usize], &edges[b as usize]);
            a.resolution
                .target_file()
                .cmp(&b.resolution.target_file())
                .then(a.fact.importing_file.cmp(&b.fact.importing_file))
                .then(a.fact.span.cmp(&b.fact.span))
                .then(a.fact.id.cmp(&b.fact.id))
        });
        let mut index = Self {
            files,
            configs,
            unattached,
            edges,
            dependents_order,
            diagnostics,
            partial,
            limits,
            components,
            digest: String::new(),
        };
        index.digest = index.compute_digest();
        index
    }

    fn compute_digest(&self) -> String {
        let bytes = serde_json::to_vec(&(
            &self.files,
            &self.configs,
            &self.unattached,
            &self.edges,
            &self.diagnostics,
            self.partial,
            &self.limits,
            &self.components,
        ))
        .unwrap_or_default();
        format!("{:x}", Sha256::digest(&bytes))
    }

    pub fn has_file(&self, path: &RootRelativePath) -> bool {
        self.files
            .binary_search_by(|file| file.path.cmp(path))
            .is_ok()
    }

    pub(crate) fn outgoing(&self, file: &RootRelativePath) -> impl Iterator<Item = &ImportEdge> {
        let start = self
            .edges
            .partition_point(|edge| edge.fact.importing_file < *file);
        let end = self
            .edges
            .partition_point(|edge| edge.fact.importing_file <= *file);
        self.edges[start..end].iter()
    }

    pub(crate) fn incoming(&self, file: &RootRelativePath) -> impl Iterator<Item = &ImportEdge> {
        let target = |position: &u32| self.edges[*position as usize].resolution.target_file();
        let start = self
            .dependents_order
            .partition_point(|position| target(position) < Some(file));
        let end = self
            .dependents_order
            .partition_point(|position| target(position) <= Some(file));
        self.dependents_order[start..end]
            .iter()
            .map(|&position| &self.edges[position as usize])
    }

    /// Runs one paged query against this index; `snapshot` names the snapshot that owns it.
    pub fn query(
        &self,
        snapshot: &SnapshotId,
        direction: Direction,
        query: &ImportQuery,
    ) -> Result<ImportPage, ImportQueryError> {
        if query.limit == 0 || query.limit > self.limits.max_page_limit {
            return Err(ImportQueryError::InvalidLimit {
                max: self.limits.max_page_limit,
            });
        }
        if direction == Direction::Dependencies && query.item.is_some() {
            return Err(ImportQueryError::InvalidQuery(
                "item is only valid for dependents".into(),
            ));
        }
        let fingerprint = fingerprint(direction, query);
        let offset = match &query.cursor {
            None => 0,
            Some(cursor) if cursor.snapshot != *snapshot => {
                return Err(ImportQueryError::ForeignCursor)
            }
            Some(cursor) if cursor.fingerprint != fingerprint => {
                return Err(ImportQueryError::InvalidCursor)
            }
            Some(cursor) => cursor.offset,
        };
        if !self.has_file(&query.file) {
            return Err(ImportQueryError::UnknownFile(query.file.clone()));
        }
        let matches = |edge: &&ImportEdge| {
            query.filter.accepts(edge, direction)
                && query
                    .item
                    .as_ref()
                    .is_none_or(|item| match &edge.resolution {
                        super::model::Resolution::ResolvedInternal { target_item, .. } => {
                            target_item.as_ref() == Some(item)
                        }
                        _ => false,
                    })
        };
        let mut selected: Vec<ImportEdge> = match direction {
            Direction::Dependencies => self
                .outgoing(&query.file)
                .filter(matches)
                .skip(offset)
                .take(query.limit + 1)
                .cloned()
                .collect(),
            Direction::Dependents => self
                .incoming(&query.file)
                .filter(matches)
                .skip(offset)
                .take(query.limit + 1)
                .cloned()
                .collect(),
        };
        let next_cursor = (selected.len() > query.limit).then(|| {
            selected.truncate(query.limit);
            ImportCursor {
                snapshot: snapshot.clone(),
                fingerprint,
                offset: offset + query.limit,
            }
        });
        Ok(ImportPage {
            snapshot: snapshot.clone(),
            edges: selected,
            next_cursor,
            partial_coverage: self.partial,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Dependencies,
    Dependents,
}

/// Narrows a query. Empty/`None` fields accept everything.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportFilter {
    pub kind: Option<ImportKind>,
    pub resolution: Vec<ResolutionKind>,
    pub staticness: Option<Staticness>,
    /// Package on the far side of the edge: the target for dependencies, the importer for dependents.
    pub other_package: Option<PackageId>,
}

impl ImportFilter {
    pub(crate) fn accepts(&self, edge: &ImportEdge, direction: Direction) -> bool {
        let other = match direction {
            Direction::Dependencies => &edge.to_package,
            Direction::Dependents => &edge.from_package,
        };
        self.kind.is_none_or(|kind| edge.fact.kind == kind)
            && self.staticness.is_none_or(|s| edge.fact.staticness == s)
            && (self.resolution.is_empty() || self.resolution.contains(&edge.resolution.kind()))
            && self
                .other_package
                .as_ref()
                .is_none_or(|package| other.as_ref() == Some(package))
    }
}

/// Position in a paged result, bound to one snapshot and one query shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportCursor {
    pub snapshot: SnapshotId,
    pub fingerprint: String,
    pub offset: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportQuery {
    pub snapshot: SnapshotId,
    pub file: RootRelativePath,
    /// Restrict dependents to imports of one item of `file`.
    pub item: Option<String>,
    pub filter: ImportFilter,
    pub cursor: Option<ImportCursor>,
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportPage {
    pub snapshot: SnapshotId,
    pub edges: Vec<ImportEdge>,
    pub next_cursor: Option<ImportCursor>,
    /// True when the snapshot's import coverage is incomplete.
    pub partial_coverage: bool,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ImportQueryError {
    #[error("snapshot not found: {0}")]
    SnapshotNotFound(String),
    #[error("limit must be between 1 and {max}")]
    InvalidLimit { max: usize },
    #[error("cursor belongs to a different snapshot")]
    ForeignCursor,
    #[error("cursor does not match this query")]
    InvalidCursor,
    #[error("file is not part of this snapshot's import index: {0:?}")]
    UnknownFile(RootRelativePath),
    #[error("invalid query: {0}")]
    InvalidQuery(String),
}

fn fingerprint(direction: Direction, query: &ImportQuery) -> String {
    let bytes = serde_json::to_vec(&(direction, &query.file, &query.item, &query.filter))
        .unwrap_or_default();
    format!("{:x}", Sha256::digest(&bytes))
}
