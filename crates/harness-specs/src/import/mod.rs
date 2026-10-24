//! Bringing a project that already uses OpenSpec or GitHub Spec Kit into this layout. A one-time,
//! non-destructive copy: the other tool's folders stay where they are, nothing that already exists
//! here is overwritten, and a change that was imported before is skipped. The conversions are
//! best effort: they keep every document and map what has an equivalent here.
//!
//! | Module | Responsibility |
//! |---|---|
//! | `openspec` | `openspec/`: project, capability specs, changes with ADDED/MODIFIED/REMOVED deltas |
//! | `speckit` | `.specify/` and `specs/NNN-feature/`: constitution, spec, plan, research, tasks |
//! | `common` | Text helpers and writing a converted change |
pub(crate) mod common;
mod openspec;
mod speckit;

use serde::Serialize;

use crate::store::{SpecError, SpecStore};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    OpenSpec,
    SpecKit,
}

/// A foreign spec folder found in the repository.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Detected {
    pub source: Source,
    /// Where it lives, relative to the repository.
    pub path: String,
    pub capabilities: usize,
    pub changes: usize,
    /// Finished changes (OpenSpec's archive) that are not imported.
    pub archived: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub project: bool,
    pub capabilities: Vec<String>,
    pub changes: Vec<String>,
    /// What was left alone and why.
    pub skipped: Vec<String>,
}

/// The foreign spec folders there are. Empty when the repository uses neither.
pub fn detect(store: &dyn SpecStore) -> Vec<Detected> {
    [openspec::detect(store), speckit::detect(store)]
        .into_iter()
        .flatten()
        .collect()
}

pub fn import(
    store: &dyn SpecStore,
    source: Source,
    date: &str,
) -> Result<ImportReport, SpecError> {
    match source {
        Source::OpenSpec => openspec::import(store, date),
        Source::SpecKit => speckit::import(store, date),
    }
}
