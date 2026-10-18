//! What one snapshot says about files, indexed once per call so every mode reads the same data.
use std::collections::{BTreeMap, HashMap};

use harness_workspace::{
    dependency_groups::Group,
    framework_relations::FrameworkRelation,
    import_graph::{ImportEdge, IndexedFile, Resolution},
    repository_intelligence::{PackageId, RepositorySnapshot, RootRelativePath},
};
use serde_json::{json, Value};

pub fn path_text(path: &RootRelativePath) -> String {
    path.as_path().display().to_string()
}

/// Indexes over one snapshot's files, edges and framework relations.
pub struct Facts<'a> {
    pub snapshot: &'a RepositorySnapshot,
    files: HashMap<&'a RootRelativePath, &'a IndexedFile>,
    packages: HashMap<&'a PackageId, &'a str>,
    outgoing: HashMap<&'a RootRelativePath, Vec<&'a ImportEdge>>,
    incoming: HashMap<&'a RootRelativePath, Vec<&'a ImportEdge>>,
    relations: HashMap<&'a RootRelativePath, Vec<&'a FrameworkRelation>>,
}

impl<'a> Facts<'a> {
    pub fn new(snapshot: &'a RepositorySnapshot) -> Self {
        let files = snapshot
            .imports
            .files
            .iter()
            .map(|file| (&file.path, file))
            .collect();
        let packages = snapshot
            .graph
            .packages
            .iter()
            .map(|package| (&package.id, package.name.as_str()))
            .collect();
        let mut outgoing: HashMap<_, Vec<_>> = HashMap::new();
        let mut incoming: HashMap<_, Vec<_>> = HashMap::new();
        for edge in &snapshot.imports.edges {
            outgoing
                .entry(&edge.fact.importing_file)
                .or_default()
                .push(edge);
            if let Some(target) = edge.resolution.target_file() {
                if target != &edge.fact.importing_file {
                    incoming.entry(target).or_default().push(edge);
                }
            }
        }
        let mut relations: HashMap<_, Vec<_>> = HashMap::new();
        for relation in &snapshot.frameworks.relations {
            relations.entry(&relation.from).or_default().push(relation);
            if let Some(to) = &relation.to {
                if to != &relation.from {
                    relations.entry(to).or_default().push(relation);
                }
            }
        }
        Self {
            snapshot,
            files,
            packages,
            outgoing,
            incoming,
            relations,
        }
    }

    pub fn file(&self, path: &RootRelativePath) -> Option<&'a IndexedFile> {
        self.files.get(path).copied()
    }

    pub fn package_name(&self, package: Option<&PackageId>) -> Option<&'a str> {
        package.and_then(|id| self.packages.get(id).copied())
    }

    pub fn package_of(&self, path: &RootRelativePath) -> Option<&'a str> {
        self.package_name(self.file(path)?.package.as_ref())
    }

    pub fn dependents_of(&self, path: &RootRelativePath) -> &[&'a ImportEdge] {
        self.incoming.get(path).map_or(&[], Vec::as_slice)
    }

    pub fn imports_of(&self, path: &RootRelativePath) -> &[&'a ImportEdge] {
        self.outgoing.get(path).map_or(&[], Vec::as_slice)
    }

    pub fn relations_of(&self, path: &RootRelativePath) -> &[&'a FrameworkRelation] {
        self.relations.get(path).map_or(&[], Vec::as_slice)
    }

    /// The frameworks active in the package that owns `path`.
    pub fn frameworks_for(&self, path: &RootRelativePath) -> Vec<String> {
        let Some(package) = self.file(path).and_then(|file| file.package.as_ref()) else {
            return Vec::new();
        };
        let mut names: Vec<String> = self
            .snapshot
            .frameworks
            .applications
            .iter()
            .filter(|application| &application.package == package)
            .map(|application| application.framework.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Imports made by `path`, one item per distinct target, in a stable order.
    pub fn import_items(&self, path: &RootRelativePath) -> Vec<Value> {
        let mut counts: BTreeMap<(&'static str, String), usize> = BTreeMap::new();
        for edge in self.imports_of(path) {
            let (kind, name) = match &edge.resolution {
                Resolution::ResolvedInternal { target_file, .. } => {
                    ("internal", path_text(target_file))
                }
                Resolution::DeclaredExternal { package } => ("external", package.clone()),
                Resolution::Possible { .. } => ("possible", edge.fact.spelling.clone()),
                Resolution::Unresolved { .. } => ("unresolved", edge.fact.spelling.clone()),
                Resolution::DynamicOrUnsupported { .. } => ("dynamic", edge.fact.spelling.clone()),
            };
            *counts.entry((kind, name)).or_default() += 1;
        }
        let order = |kind: &str| {
            ["internal", "external", "possible", "unresolved", "dynamic"]
                .iter()
                .position(|k| *k == kind)
        };
        let mut items: Vec<_> = counts.into_iter().collect();
        items.sort_by(|a, b| {
            order(a.0 .0)
                .cmp(&order(b.0 .0))
                .then(b.1.cmp(&a.1))
                .then(a.0 .1.cmp(&b.0 .1))
        });
        items
            .into_iter()
            .map(|((kind, name), count)| json!({"kind": kind, "name": name, "imports": count}))
            .collect()
    }

    /// Files that import `path`, with how many imports each makes, most first.
    pub fn dependent_files(&self, path: &RootRelativePath) -> Vec<(RootRelativePath, usize)> {
        let mut counts: BTreeMap<&RootRelativePath, usize> = BTreeMap::new();
        for edge in self.dependents_of(path) {
            *counts.entry(&edge.fact.importing_file).or_default() += 1;
        }
        let mut files: Vec<_> = counts
            .into_iter()
            .map(|(file, count)| (file.clone(), count))
            .collect();
        files.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        files
    }

    /// A relation as a small item seen from `path`.
    pub fn relation_item(relation: &FrameworkRelation, path: &RootRelativePath) -> Value {
        let registers = &relation.from == path;
        json!({
            "kind": relation.kind,
            "framework": relation.framework,
            "label": relation.label,
            "certainty": relation.certainty,
            "role": if registers { "registers" } else { "receives" },
            "other_file": if registers { relation.to.as_ref().map(path_text) } else { Some(path_text(&relation.from)) },
            "rule": format!("{} v{}", relation.rule.id, relation.rule.version),
        })
    }

    /// Imports that cross a folder's edge, counted by the package (or top folder) on the other
    /// side: `(going out, coming in)`, most first.
    pub fn crossings(&self, group: &Group) -> (Crossings, Crossings) {
        let side = |file: &RootRelativePath| {
            self.package_of(file).map_or_else(
                || {
                    file.as_path()
                        .components()
                        .next()
                        .map_or(String::new(), |c| {
                            c.as_os_str().to_string_lossy().into_owned()
                        })
                },
                str::to_owned,
            )
        };
        let (mut out, mut incoming): (BTreeMap<String, usize>, BTreeMap<String, usize>) =
            Default::default();
        for member in &group.members {
            for edge in self.imports_of(member) {
                if let Some(target) = edge.resolution.target_file().filter(|t| !group.contains(t)) {
                    *out.entry(side(target)).or_default() += 1;
                }
            }
            for edge in self.dependents_of(member) {
                if !group.contains(&edge.fact.importing_file) {
                    *incoming.entry(side(&edge.fact.importing_file)).or_default() += 1;
                }
            }
        }
        let ranked = |map: BTreeMap<String, usize>| {
            let mut items: Vec<_> = map.into_iter().collect();
            items.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            items
        };
        (ranked(out), ranked(incoming))
    }
}

/// Counts per package (or top folder) on the other side of a folder's edge, most first.
pub type Crossings = Vec<(String, usize)>;
