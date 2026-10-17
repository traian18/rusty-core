//! Indexed, read-only lookups over one repository snapshot, built once and shared by the rules.
use std::collections::{BTreeSet, HashMap, VecDeque};

use harness_workspace::{
    import_graph::{ImportEdge, ImportFactId},
    repository_intelligence::{Package, PackageId, RepositorySnapshot, RootRelativePath},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Walk {
    /// Follow imports from importer to imported.
    Imports,
    /// Follow imports from imported to importer.
    ImportedBy,
}

pub struct RepoView<'a> {
    pub snapshot: &'a RepositorySnapshot,
    packages: HashMap<&'a PackageId, &'a Package>,
    file_package: HashMap<&'a RootRelativePath, Option<&'a PackageId>>,
    incoming: HashMap<&'a RootRelativePath, Vec<&'a ImportEdge>>,
    outgoing: HashMap<&'a RootRelativePath, Vec<&'a ImportEdge>>,
    by_fact: HashMap<&'a ImportFactId, &'a ImportEdge>,
}

impl<'a> RepoView<'a> {
    pub fn new(snapshot: &'a RepositorySnapshot) -> Self {
        let mut incoming: HashMap<&RootRelativePath, Vec<&ImportEdge>> = HashMap::new();
        let mut outgoing: HashMap<&RootRelativePath, Vec<&ImportEdge>> = HashMap::new();
        let mut by_fact = HashMap::new();
        for edge in &snapshot.imports.edges {
            by_fact.insert(&edge.fact.id, edge);
            if let Some(target) = edge.resolution.target_file() {
                if *target != edge.fact.importing_file {
                    outgoing
                        .entry(&edge.fact.importing_file)
                        .or_default()
                        .push(edge);
                    incoming.entry(target).or_default().push(edge);
                }
            }
        }
        Self {
            snapshot,
            packages: snapshot.graph.packages.iter().map(|p| (&p.id, p)).collect(),
            file_package: snapshot
                .imports
                .files
                .iter()
                .map(|file| (&file.path, file.package.as_ref()))
                .collect(),
            incoming,
            outgoing,
            by_fact,
        }
    }

    pub fn has_file(&self, file: &RootRelativePath) -> bool {
        self.file_package.contains_key(file)
    }

    pub fn package(&self, id: &PackageId) -> Option<&'a Package> {
        self.packages.get(id).copied()
    }

    pub fn package_of_file(&self, file: &RootRelativePath) -> Option<&'a Package> {
        self.file_package
            .get(file)
            .copied()
            .flatten()
            .and_then(|id| self.package(id))
    }

    /// Resolved internal imports of `file`, excluding self-imports.
    pub fn outgoing(&self, file: &RootRelativePath) -> &[&'a ImportEdge] {
        self.outgoing.get(file).map_or(&[], Vec::as_slice)
    }

    /// Resolved internal imports that target `file`, excluding self-imports.
    pub fn incoming(&self, file: &RootRelativePath) -> &[&'a ImportEdge] {
        self.incoming.get(file).map_or(&[], Vec::as_slice)
    }

    pub fn edge(&self, fact: &ImportFactId) -> Option<&'a ImportEdge> {
        self.by_fact.get(fact).copied()
    }

    /// Files reachable from `start` in at most `hops` steps (excluding `start` itself).
    pub fn reachable(
        &self,
        start: &RootRelativePath,
        hops: usize,
        walk: Walk,
    ) -> BTreeSet<RootRelativePath> {
        let mut seen = BTreeSet::from([start.clone()]);
        let mut frontier = VecDeque::from([(start.clone(), 0usize)]);
        while let Some((file, depth)) = frontier.pop_front() {
            if depth == hops {
                continue;
            }
            let next = match walk {
                Walk::Imports => self
                    .outgoing(&file)
                    .iter()
                    .filter_map(|e| e.resolution.target_file())
                    .cloned()
                    .collect::<Vec<_>>(),
                Walk::ImportedBy => self
                    .incoming(&file)
                    .iter()
                    .map(|e| e.fact.importing_file.clone())
                    .collect(),
            };
            for neighbour in next {
                if seen.insert(neighbour.clone()) {
                    frontier.push_back((neighbour, depth + 1));
                }
            }
        }
        seen.remove(start);
        seen
    }
}
