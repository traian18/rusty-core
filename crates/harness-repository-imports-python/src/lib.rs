#![warn(clippy::all)]

//! Python import extraction, resolution and project-file reading for the repository graph.
//!
//! Reads `pyproject.toml`, `setup.cfg`, `Pipfile` and `requirements*.txt` as text, and resolves
//! `import` and `from … import` against the modules found in the repository. Nothing is executed.

mod extract;
mod manifest;
mod resolve;
mod stdlib;

use std::sync::Arc;

use harness_workspace::import_graph::ImportSupport;

pub use extract::PythonImportAdapter;
pub use manifest::manifest_adapters;
pub use resolve::PythonImportResolver;

/// File extensions the language's sources use, to register with the repository builder.
pub const SOURCE_EXTENSIONS: &[&str] = &["py"];

/// The adapter and resolver pair for `RepositoryGraphBuilder::import_support`.
pub fn import_support() -> ImportSupport {
    ImportSupport {
        adapter: Arc::new(PythonImportAdapter),
        resolver: Arc::new(PythonImportResolver),
    }
}
