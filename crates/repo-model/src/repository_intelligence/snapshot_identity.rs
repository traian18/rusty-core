//! Deterministic snapshot identity.
//!
//! The id covers every semantic input: discovered paths, package facts, limits,
//! diagnostics, omitted work, and the contents of manifests and source roots.
//! The byte layout is stable so unchanged repositories keep their ids.
use std::{collections::BTreeSet, path::Path};

use sha2::{Digest, Sha256};

use super::{
    discovery::read_contained_file,
    ids::SnapshotId,
    model::{DiscoveryDiagnostic, DiscoveryError, Package, RepositoryStructure},
};

const IDENTITY_VERSION: &str = "repository-intelligence-v6:adapter=cargo-package-json-v4";

/// Snapshot identity is deterministic over discovered paths, facts, and effective limits.
pub fn snapshot_id(graph: &RepositoryStructure, root: &Path) -> Result<SnapshotId, DiscoveryError> {
    let mut hasher = IdentityHasher::new(format!("{IDENTITY_VERSION}:{}", graph.id.0));
    hasher.text(&format!("{:?}", graph.effective_limits));
    hasher.number(graph.omitted_entries);
    hasher.number(graph.packages.len());
    hasher.number(graph.workspace_members.len());
    hasher.number(graph.diagnostics.len());
    hash_diagnostics(&mut hasher, &graph.diagnostics);
    for member in &graph.workspace_members {
        hasher.path(member.as_path());
    }
    let mut packages = graph.packages.iter().collect::<Vec<_>>();
    packages.sort_by(|a, b| a.manifest.cmp(&b.manifest).then(a.name.cmp(&b.name)));
    let mut files_to_hash = BTreeSet::new();
    for package in packages {
        hash_package(&mut hasher, package);
        files_to_hash.insert(package.manifest.as_path().to_path_buf());
        files_to_hash.extend(
            package
                .facts
                .source_roots
                .iter()
                .map(|path| path.as_path().to_path_buf()),
        );
    }
    for relative in files_to_hash {
        let contents = read_contained_file(root, &relative, u64::MAX)
            .map_err(|error| DiscoveryError::Failed(error.to_string()))?;
        hasher.path(&relative);
        hasher.bytes(&contents);
    }
    Ok(hasher.finish())
}

fn hash_diagnostics(hasher: &mut IdentityHasher, diagnostics: &[DiscoveryDiagnostic]) {
    let mut sorted = diagnostics.iter().collect::<Vec<_>>();
    sorted.sort_by(|a, b| a.path.cmp(&b.path).then(a.message.cmp(&b.message)));
    for diagnostic in sorted {
        let path = diagnostic
            .path
            .as_ref()
            .map(|path| path.as_path().to_string_lossy().into_owned())
            .unwrap_or_default();
        hasher.text(&path);
        hasher.text(&diagnostic.message);
    }
}

fn hash_package(hasher: &mut IdentityHasher, package: &Package) {
    hasher.text(&package.id.0);
    hasher.text(&package.name);
    hasher.path(package.manifest.as_path());
    let facts = &package.facts;
    for fact in &facts.declared_dependencies {
        hasher.tagged("dependency", fact);
    }
    for fact in &facts.dev_dependencies {
        hasher.tagged("dev-dependency", fact);
    }
    for fact in &facts.inferred_facts {
        hasher.tagged("inferred", fact);
    }
    for fact in &facts.source_imports {
        hasher.tagged("source-import", fact);
    }
    for path in &facts.source_roots {
        hasher.tagged_path("source-root", path.as_path());
    }
    for path in &facts.entry_points {
        hasher.tagged_path("entry-point", path.as_path());
    }
    for path in &facts.target_roots {
        hasher.tagged_path("target-root", path.as_path());
    }
    for path in &facts.workspace_members {
        hasher.tagged_path("package-workspace-member", path.as_path());
    }
}

/// Length-prefixed field encoding so adjacent fields can never run together.
struct IdentityHasher {
    input: Vec<u8>,
}

impl IdentityHasher {
    fn new(prefix: String) -> Self {
        Self {
            input: prefix.into_bytes(),
        }
    }

    fn bytes(&mut self, value: &[u8]) {
        self.input
            .extend_from_slice(&(value.len() as u64).to_le_bytes());
        self.input.extend_from_slice(value);
    }

    fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn number(&mut self, value: usize) {
        self.bytes(&(value as u64).to_le_bytes());
    }

    fn path(&mut self, value: &Path) {
        self.text(&value.to_string_lossy());
    }

    fn tagged(&mut self, tag: &str, value: &str) {
        self.text(tag);
        self.text(value);
    }

    fn tagged_path(&mut self, tag: &str, value: &Path) {
        self.text(tag);
        self.path(value);
    }

    fn finish(self) -> SnapshotId {
        SnapshotId(format!("{:x}", Sha256::digest(&self.input)))
    }
}
