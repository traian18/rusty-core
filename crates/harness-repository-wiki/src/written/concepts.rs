//! The ideas a reader has to learn: the central types (and, where a file has none, functions) of
//! the most depended-on files, spread across the top-level folders so one subsystem does not take
//! every slot, with the import links between them.
use std::collections::{BTreeMap, BTreeSet};

use harness_workspace::repository_intelligence::RepositorySnapshot;

use super::{evidence::Evidence, excerpts};
use crate::{context::Centrality, facts::Facts, tests_of::is_test_file};

const CANDIDATE_FILES: usize = 24;
const PER_FILE: usize = 2;
const EXCERPT_LINES: u32 = 40;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Concept {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub from: String,
    pub to: String,
}

/// Source files, most depended-on first, without tests.
pub fn central_files(
    snapshot: &RepositorySnapshot,
    centrality: &Centrality,
    limit: usize,
) -> Vec<String> {
    let mut files: Vec<_> = snapshot
        .imports
        .files
        .iter()
        .filter(|f| !is_test_file(f.path.as_path()))
        .collect();
    files.sort_by(|a, b| {
        centrality
            .score(&b.path)
            .total_cmp(&centrality.score(&a.path))
            .then(a.path.cmp(&b.path))
    });
    files
        .into_iter()
        .take(limit)
        .map(|f| f.path.as_path().display().to_string())
        .collect()
}

fn top_folder(path: &str) -> &str {
    path.split('/').next().unwrap_or(path)
}

/// Up to `max` concepts with their excerpts added to `evidence`, and the links between them.
pub fn choose(
    snapshot: &RepositorySnapshot,
    centrality: &Centrality,
    evidence: &mut Evidence<'_>,
    max: usize,
) -> (Vec<Concept>, Vec<Link>) {
    let mut by_folder: BTreeMap<String, Vec<Concept>> = BTreeMap::new();
    let mut folder_order: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for path in central_files(snapshot, centrality, CANDIDATE_FILES) {
        let Some(text) = evidence.read(&path) else {
            continue;
        };
        let symbols = excerpts::symbols(&path, &text);
        let mut picked: Vec<_> = symbols.iter().filter(|s| s.class).take(PER_FILE).collect();
        if picked.is_empty() {
            picked = symbols.iter().take(1).collect();
        }
        for symbol in picked {
            if !seen.insert(symbol.name.to_lowercase()) {
                continue;
            }
            let folder = top_folder(&path).to_owned();
            if !by_folder.contains_key(&folder) {
                folder_order.push(folder.clone());
            }
            by_folder.entry(folder).or_default().push(Concept {
                name: symbol.name.clone(),
                path: path.clone(),
            });
        }
    }
    // Round-robin across folders, most central folder first.
    let mut chosen = Vec::new();
    let mut round = 0;
    while chosen.len() < max {
        let mut added = false;
        for folder in &folder_order {
            if let Some(concept) = by_folder.get(folder).and_then(|list| list.get(round)) {
                if chosen.len() < max {
                    chosen.push(concept.clone());
                    added = true;
                }
            }
        }
        if !added {
            break;
        }
        round += 1;
    }
    for concept in &chosen {
        if let Some(text) = evidence.read(&concept.path) {
            if let Some(symbol) = excerpts::symbols(&concept.path, &text)
                .into_iter()
                .find(|s| s.name == concept.name)
            {
                evidence.lines(
                    &concept.path,
                    symbol.start,
                    symbol.end,
                    EXCERPT_LINES,
                    &format!("`{}`", concept.name),
                );
            }
        }
        evidence.mention(&concept.name);
    }
    let facts = Facts::new(snapshot);
    let mut links = Vec::new();
    for a in &chosen {
        let Ok(path) = harness_workspace::repository_intelligence::RootRelativePath::new(&a.path)
        else {
            continue;
        };
        let imported: BTreeSet<String> = facts
            .imports_of(&path)
            .iter()
            .filter_map(|edge| match &edge.resolution {
                harness_workspace::import_graph::Resolution::ResolvedInternal {
                    target_file,
                    ..
                } => Some(target_file.as_path().display().to_string()),
                _ => None,
            })
            .collect();
        for b in &chosen {
            if a.path != b.path && imported.contains(&b.path) {
                links.push(Link {
                    from: a.name.clone(),
                    to: b.name.clone(),
                });
            }
        }
    }
    (chosen, links)
}
