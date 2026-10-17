#![warn(clippy::all)]

//! PHP import extraction, resolution and `composer.json` reading for the repository graph.
//!
//! `use` is matched to files by `namespace` and class declarations; literal `require` and
//! `include` paths are followed. Composer is never run.

mod extract;
mod manifest;
mod resolve;

use std::sync::Arc;

use harness_workspace::import_graph::ImportSupport;

pub use extract::PhpImportAdapter;
pub use manifest::manifest_adapters;
pub use resolve::PhpImportResolver;

/// File extensions the language's sources use, to register with the repository builder.
pub const SOURCE_EXTENSIONS: &[&str] = &["php"];

/// The adapter and resolver pair for `RepositoryGraphBuilder::import_support`.
pub fn import_support() -> ImportSupport {
    ImportSupport {
        adapter: Arc::new(PhpImportAdapter),
        resolver: Arc::new(PhpImportResolver),
    }
}
