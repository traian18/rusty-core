//! One path through the system for "How it works": an entry point, then the most central file it
//! imports, then the most central file that one imports, and so on.
use std::collections::BTreeSet;

use harness_workspace::{
    framework_relations::RelationKind,
    import_graph::Resolution,
    repository_intelligence::{RepositorySnapshot, RootRelativePath},
};

use crate::{context::Centrality, facts::Facts, tests_of::is_test_file};

const ENTRY_STEMS: [&str; 8] = [
    "main", "index", "app", "cli", "server", "__main__", "lib", "mod",
];
pub const MAX_HOPS: usize = 6;

/// What kind of program the entry suggests, as a hint for the narrative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Archetype {
    Service,
    Cli,
    Library,
    Application,
}

impl Archetype {
    pub fn name(self) -> &'static str {
        match self {
            Self::Service => "service (it answers requests)",
            Self::Cli => "command-line tool or desktop app (it runs commands)",
            Self::Library => "library (others call into it)",
            Self::Application => "application",
        }
    }
}

fn stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.split('.').next().unwrap_or(name)
}

fn imports(facts: &Facts<'_>, path: &RootRelativePath) -> Vec<RootRelativePath> {
    let mut out: Vec<RootRelativePath> = facts
        .imports_of(path)
        .iter()
        .filter_map(|edge| match &edge.resolution {
            Resolution::ResolvedInternal { target_file, .. } => Some(target_file.clone()),
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The best entry file and what it suggests, or `None` when nothing looks like one.
pub fn entry(
    snapshot: &RepositorySnapshot,
    facts: &Facts<'_>,
) -> Option<(RootRelativePath, Archetype)> {
    let mut best: Option<(usize, RootRelativePath, Archetype)> = None;
    for file in &snapshot.imports.files {
        let path = &file.path;
        let text = path.as_path().display().to_string();
        if is_test_file(path.as_path()) {
            continue;
        }
        let relations = facts.relations_of(path);
        let routes = relations
            .iter()
            .any(|r| &r.from == path && r.kind == RelationKind::RouteRegisteredTo);
        let commands = relations
            .iter()
            .any(|r| &r.from == path && r.kind == RelationKind::CommandExposedAs);
        let named = ENTRY_STEMS.contains(&stem(&text));
        let unimported = facts.dependent_files(path).is_empty();
        if !(routes || commands || (named && unimported)) {
            continue;
        }
        let archetype = if routes {
            Archetype::Service
        } else if commands
            || stem(&text) == "main"
            || stem(&text) == "__main__"
            || stem(&text) == "cli"
        {
            Archetype::Cli
        } else if matches!(stem(&text), "lib" | "index" | "mod") {
            Archetype::Library
        } else {
            Archetype::Application
        };
        let reach = imports(facts, path).len() + if routes || commands { 100 } else { 0 };
        if best
            .as_ref()
            .is_none_or(|(score, p, _)| reach > *score || (reach == *score && path < p))
        {
            best = Some((reach, path.clone(), archetype));
        }
    }
    best.map(|(_, path, archetype)| (path, archetype))
}

/// The entry, then at each step the most central file not yet visited that the current one imports.
pub fn trace(
    facts: &Facts<'_>,
    centrality: &Centrality,
    start: &RootRelativePath,
) -> Vec<RootRelativePath> {
    let mut path = vec![start.clone()];
    let mut visited: BTreeSet<RootRelativePath> = BTreeSet::from([start.clone()]);
    let mut current = start.clone();
    while path.len() < MAX_HOPS {
        let next = imports(facts, &current)
            .into_iter()
            .filter(|p| !visited.contains(p) && !is_test_file(p.as_path()))
            .max_by(|a, b| {
                centrality
                    .score(a)
                    .total_cmp(&centrality.score(b))
                    .then(b.cmp(a))
            });
        let Some(next) = next else { break };
        visited.insert(next.clone());
        path.push(next.clone());
        current = next;
    }
    path
}
