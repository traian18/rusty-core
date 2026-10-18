//! What the graph can say about a path that is not an indexed source file: its language when it
//! is a known one that is not supported, why it was left out, and what is on disk now.
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::discovery::ignored_directory_in;

/// What is on disk at a path inside the authorized root, read without following symlinks.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PathState {
    /// A regular file, with the hash the import index records for unchanged content.
    File {
        content_hash: String,
    },
    Directory,
    Missing,
    /// This graph cannot read the disk (for example a fake), or the file was unreadable or too large.
    Unknown,
}

/// Extensions of languages Rusty knows by name but does not read, so a tool can say "Ruby is not
/// supported" instead of "unknown file". Support is decided by the registered adapters, never by
/// this list.
const KNOWN_UNSUPPORTED: [(&str, &str); 22] = [
    ("rb", "Ruby"),
    ("kt", "Kotlin"),
    ("kts", "Kotlin"),
    ("swift", "Swift"),
    ("scala", "Scala"),
    ("dart", "Dart"),
    ("lua", "Lua"),
    ("ex", "Elixir"),
    ("exs", "Elixir"),
    ("erl", "Erlang"),
    ("hs", "Haskell"),
    ("clj", "Clojure"),
    ("r", "R"),
    ("jl", "Julia"),
    ("pl", "Perl"),
    ("zig", "Zig"),
    ("nim", "Nim"),
    ("sh", "Shell"),
    ("m", "Objective-C"),
    ("mm", "Objective-C++"),
    ("vue", "Vue single-file component"),
    ("svelte", "Svelte"),
];

/// The language of a path whose extension is known but not supported.
pub fn known_unsupported_language(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    KNOWN_UNSUPPORTED
        .iter()
        .find(|(known, _)| *known == extension)
        .map(|(_, language)| *language)
}

/// The ignored directory (such as `node_modules`) a path lies under, if any.
pub fn ignored_directory(path: &Path) -> Option<&'static str> {
    ignored_directory_in(path)
}

/// Extensions and names reported by the unsupported table, for capability listings.
pub fn known_unsupported_languages() -> Vec<(&'static str, &'static str)> {
    KNOWN_UNSUPPORTED.to_vec()
}

/// The hash the import index uses for file content.
pub fn content_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
