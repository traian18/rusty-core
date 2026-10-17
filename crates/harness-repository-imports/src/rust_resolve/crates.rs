//! Meaning of a crate name used as the first segment of a Rust path.
use std::collections::HashMap;

use harness_workspace::repository_intelligence::{Package, PackageId, RepositoryStructure};

use crate::paths::file_name;

const STD_CRATES: [&str; 5] = ["std", "core", "alloc", "proc_macro", "test"];

/// What a crate name refers to from inside one package.
pub(super) enum CrateLookup {
    /// The package's own library.
    Own(PackageId),
    /// A declared dependency that is another workspace package.
    Sibling(PackageId),
    /// A declared dependency whose name matches several workspace packages.
    Ambiguous(Vec<PackageId>),
    /// A declared dependency outside the workspace (carries the manifest spelling).
    External(String),
    /// A crate shipped with the toolchain.
    Std,
    Unknown,
}

pub(super) struct CrateIndex<'a> {
    packages: HashMap<&'a PackageId, &'a Package>,
    by_crate_name: HashMap<String, Vec<PackageId>>,
}

/// Cargo spells crates with `-` in manifests and `_` in source.
fn normalize(name: &str) -> String {
    name.replace('-', "_")
}

impl<'a> CrateIndex<'a> {
    pub fn new(structure: &'a RepositoryStructure) -> Self {
        let mut by_crate_name: HashMap<String, Vec<PackageId>> = HashMap::new();
        for package in structure
            .packages
            .iter()
            .filter(|p| file_name(&p.manifest) == "Cargo.toml")
        {
            by_crate_name
                .entry(normalize(&package.name))
                .or_default()
                .push(package.id.clone());
        }
        Self {
            packages: structure.packages.iter().map(|p| (&p.id, p)).collect(),
            by_crate_name,
        }
    }

    pub fn package(&self, id: &PackageId) -> Option<&'a Package> {
        self.packages.get(id).copied()
    }

    pub fn locate(&self, from: Option<&Package>, name: &str) -> CrateLookup {
        let normalized = normalize(name);
        let std_or_unknown = || {
            if STD_CRATES.contains(&name) {
                CrateLookup::Std
            } else {
                CrateLookup::Unknown
            }
        };
        let Some(package) = from else {
            return std_or_unknown();
        };
        if normalize(&package.name) == normalized {
            return CrateLookup::Own(package.id.clone());
        }
        let Some(declared) = package
            .facts
            .declared_dependencies
            .iter()
            .find(|dependency| normalize(dependency) == normalized)
        else {
            return std_or_unknown();
        };
        let siblings = self
            .by_crate_name
            .get(&normalized)
            .into_iter()
            .flatten()
            .filter(|id| **id != package.id)
            .cloned()
            .collect::<Vec<_>>();
        match siblings.len() {
            0 => CrateLookup::External(declared.clone()),
            1 => CrateLookup::Sibling(siblings[0].clone()),
            _ => CrateLookup::Ambiguous(siblings),
        }
    }
}
