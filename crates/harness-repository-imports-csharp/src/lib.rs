#![warn(clippy::all)]

//! C# import extraction, resolution and `.csproj` reading for the repository graph.
//!
//! A `using` names a namespace, so edges are namespace-level: they point at the files that declare
//! the namespace. MSBuild is never run.

mod extract;
mod manifest;
mod resolve;

use std::sync::Arc;

use harness_workspace::import_graph::ImportSupport;

pub use extract::CSharpImportAdapter;
pub use manifest::manifest_adapters;
pub use resolve::CSharpImportResolver;

/// File extensions the language's sources use, to register with the repository builder.
pub const SOURCE_EXTENSIONS: &[&str] = &["cs"];

/// The adapter and resolver pair for `RepositoryGraphBuilder::import_support`.
pub fn import_support() -> ImportSupport {
    ImportSupport {
        adapter: Arc::new(CSharpImportAdapter),
        resolver: Arc::new(CSharpImportResolver),
    }
}
