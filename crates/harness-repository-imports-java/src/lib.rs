#![warn(clippy::all)]

//! Java import extraction, resolution and Maven/Gradle file reading for the repository graph.
//!
//! Imports are matched to files by the `package` declaration and top-level types of each source
//! file. Neither Maven nor Gradle is run.

mod extract;
mod manifest;
mod resolve;

use std::sync::Arc;

use harness_workspace::import_graph::ImportSupport;

pub use extract::JavaImportAdapter;
pub use manifest::manifest_adapters;
pub use resolve::JavaImportResolver;

/// File extensions the language's sources use, to register with the repository builder.
pub const SOURCE_EXTENSIONS: &[&str] = &["java"];

/// The adapter and resolver pair for `RepositoryGraphBuilder::import_support`.
pub fn import_support() -> ImportSupport {
    ImportSupport {
        adapter: Arc::new(JavaImportAdapter),
        resolver: Arc::new(JavaImportResolver),
    }
}
