#![warn(clippy::all)]

//! C and C++ include extraction, resolution and CMake / compile-database reading for the
//! repository graph. Both languages share one crate because `#include` is the same directive
//! and the lookup rules are the same. Neither CMake nor a compiler is run.

mod extract;
mod headers;
mod manifest;
mod resolve;

use std::sync::Arc;

use harness_workspace::import_graph::ImportSupport;

pub use extract::{NativeImportAdapter, CPP_EXTENSIONS, C_EXTENSIONS};
pub use manifest::manifest_adapters;
pub use resolve::NativeImportResolver;

/// File extensions the languages' sources use, to register with the repository builder.
pub fn source_extensions() -> Vec<&'static str> {
    C_EXTENSIONS.iter().chain(CPP_EXTENSIONS).copied().collect()
}

/// The adapter and resolver pair for `RepositoryGraphBuilder::import_support`.
pub fn import_support() -> ImportSupport {
    ImportSupport {
        adapter: Arc::new(NativeImportAdapter),
        resolver: Arc::new(NativeImportResolver),
    }
}
