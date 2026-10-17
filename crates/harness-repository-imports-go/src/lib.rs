#![warn(clippy::all)]

//! Go import extraction, resolution and `go.mod` reading for the repository graph.
//!
//! A Go package is a directory: an import resolves to the directory of a module found in the
//! repository, and the edge points at that package's lead file. Nothing is executed.

mod extract;
mod manifest;
mod resolve;

use std::sync::Arc;

use harness_workspace::import_graph::ImportSupport;

pub use extract::GoImportAdapter;
pub use manifest::manifest_adapters;
pub use resolve::GoImportResolver;

/// File extensions the language's sources use, to register with the repository builder.
pub const SOURCE_EXTENSIONS: &[&str] = &["go"];

/// The adapter and resolver pair for `RepositoryGraphBuilder::import_support`.
pub fn import_support() -> ImportSupport {
    ImportSupport {
        adapter: Arc::new(GoImportAdapter),
        resolver: Arc::new(GoImportResolver),
    }
}
