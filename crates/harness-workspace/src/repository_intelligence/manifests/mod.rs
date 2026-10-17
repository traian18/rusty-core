//! Manifest adapters: one per package-manager file format.
mod cargo;
mod package_json;

use std::sync::Arc;

pub use cargo::CargoManifestAdapter;
pub use package_json::PackageJsonAdapter;

use crate::repository_intelligence::ManifestAdapter;

/// The adapters Rusty supports out of the box: Cargo and npm-style `package.json`.
pub fn default_manifest_adapters() -> Vec<Arc<dyn ManifestAdapter>> {
    vec![Arc::new(CargoManifestAdapter), Arc::new(PackageJsonAdapter)]
}
