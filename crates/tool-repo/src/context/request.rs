//! The `repo_context` arguments, validated before anything is queried.
use serde::Deserialize;
use serde_json::{json, Value};

use crate::failure::Failure;

pub(super) const DEFAULT_BUDGET: usize = 6_000;
pub(super) const MIN_BUDGET: usize = 1_000;
pub(super) const MAX_BUDGET: usize = 20_000;
pub(super) const DEFAULT_DEPTH: usize = 2;
pub(super) const MAX_DEPTH: usize = 5;
/// Most paths a `changes` request may list.
pub(super) const MAX_TARGETS: usize = 64;
/// Depth cap when the target is a whole folder or a set of changed files.
pub(super) const WIDE_DEPTH: usize = 2;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Mode {
    Connections,
    BlastRadius,
    Orient,
    Changes,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub(super) enum Section {
    Frameworks,
    Findings,
    History,
    Tests,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    target: Option<String>,
    targets: Option<Vec<String>>,
    mode: Option<Mode>,
    depth: Option<usize>,
    budget: Option<usize>,
    include: Option<Vec<Section>>,
    snapshot: Option<String>,
}

pub(super) struct Request {
    pub targets: Vec<String>,
    pub mode: Mode,
    pub depth: usize,
    pub budget: usize,
    pub include: Vec<Section>,
    /// The caller listed sections; only then is an unavailable one reported as missing.
    pub explicit_include: bool,
    pub snapshot: Option<String>,
}

impl Request {
    pub fn includes(&self, section: Section) -> bool {
        self.include.contains(&section)
    }
}

pub(super) fn schema() -> Value {
    json!({"type":"object","properties":{
        "target":{"type":"string","description":"A workspace-relative file or folder path, a node id (dir:<path>, a package group id, rest:<focus>), or empty for the repository root"},
        "targets":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":MAX_TARGETS,"description":"Changed files, for mode changes"},
        "mode":{"type":"string","enum":["connections","blast_radius","orient","changes"],"description":"Default connections"},
        "depth":{"type":"integer","minimum":1,"maximum":MAX_DEPTH,"description":"Transitive depth for blast_radius and changes; default 2"},
        "budget":{"type":"integer","minimum":MIN_BUDGET,"maximum":MAX_BUDGET,"description":"Approximate characters of result; default 6000"},
        "include":{"type":"array","items":{"type":"string","enum":["frameworks","findings","history","tests"]}},
        "snapshot":{"type":"string"}},
        "additionalProperties":false})
}

pub(super) fn parse(arguments: Value) -> Result<Request, Failure> {
    let raw: Raw = crate::args::parse(arguments)?;
    let mode = raw.mode.unwrap_or(Mode::Connections);
    let targets = match (mode, raw.target, raw.targets) {
        (Mode::Changes, target, targets) => {
            let mut all = targets.unwrap_or_default();
            all.extend(target);
            if all.is_empty() {
                return Err(Failure::invalid(
                    "changes needs targets (the changed files)",
                ));
            }
            if all.len() > MAX_TARGETS {
                return Err(Failure::new(
                    "limit_exceeded",
                    format!("at most {MAX_TARGETS} targets"),
                ));
            }
            all
        }
        (_, _, Some(_)) => return Err(Failure::invalid("targets is only for mode changes")),
        (_, target, None) => vec![target.unwrap_or_default()],
    };
    let depth = raw.depth.unwrap_or(DEFAULT_DEPTH);
    if depth == 0 || depth > MAX_DEPTH {
        return Err(Failure::new(
            "limit_exceeded",
            format!("depth must be between 1 and {MAX_DEPTH}"),
        ));
    }
    let budget = raw.budget.unwrap_or(DEFAULT_BUDGET);
    if !(MIN_BUDGET..=MAX_BUDGET).contains(&budget) {
        return Err(Failure::new(
            "limit_exceeded",
            format!("budget must be between {MIN_BUDGET} and {MAX_BUDGET}"),
        ));
    }
    let explicit_include = raw.include.is_some();
    let mut include = raw.include.unwrap_or_else(|| {
        vec![
            Section::Frameworks,
            Section::Findings,
            Section::History,
            Section::Tests,
        ]
    });
    include.sort();
    include.dedup();
    Ok(Request {
        targets,
        mode,
        depth,
        budget,
        include,
        explicit_include,
        snapshot: raw.snapshot,
    })
}
