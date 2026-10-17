//! Locating script files the way TypeScript does: exact name, `.js`→`.ts` mapping, extensions, `index`.
use std::{collections::HashSet, path::Path};

use harness_workspace::repository_intelligence::RootRelativePath;

use crate::paths::join_normalized;

const EXTENSIONS: [&str; 9] = ["ts", "tsx", "d.ts", "js", "jsx", "mts", "mjs", "cts", "cjs"];
const ASSET_EXTENSIONS: [&str; 12] = [
    "json", "css", "scss", "sass", "less", "svg", "png", "jpg", "jpeg", "gif", "html", "wasm",
];
/// JavaScript spellings that TypeScript maps to a TypeScript source.
const JS_TO_TS: [(&str, &[&str]); 4] = [
    (".js", &[".ts", ".tsx"]),
    (".jsx", &[".tsx"]),
    (".mjs", &[".mts"]),
    (".cjs", &[".cts"]),
];

/// The set of indexed TypeScript/JavaScript files, answering "does this path exist".
pub(crate) struct ScriptFiles<'a> {
    files: HashSet<&'a RootRelativePath>,
}

impl<'a> ScriptFiles<'a> {
    pub fn new(files: impl Iterator<Item = &'a RootRelativePath>) -> Self {
        Self {
            files: files.collect(),
        }
    }

    /// Resolves `specifier` relative to `base`, probing extensions and `index` files.
    pub fn probe(&self, base: &Path, specifier: &str) -> Option<RootRelativePath> {
        let joined = join_normalized(base, specifier)?;
        self.probe_path(joined.as_path())
    }

    /// Probes an already-joined path in priority order; the first existing file wins.
    pub fn probe_path(&self, path: &Path) -> Option<RootRelativePath> {
        probe_candidates(&path.to_string_lossy())
            .into_iter()
            .filter_map(|candidate| RootRelativePath::new(candidate).ok())
            .find(|candidate| self.files.contains(candidate))
    }
}

fn probe_candidates(text: &str) -> Vec<String> {
    let mut candidates = vec![text.to_owned()];
    for (from, targets) in JS_TO_TS {
        if let Some(stem) = text.strip_suffix(from) {
            candidates.extend(targets.iter().map(|target| format!("{stem}{target}")));
        }
    }
    candidates.extend(EXTENSIONS.iter().map(|ext| format!("{text}.{ext}")));
    candidates.extend(EXTENSIONS.iter().map(|ext| format!("{text}/index.{ext}")));
    candidates
}

/// True for specifiers that name non-script assets (`./logo.svg`, `./data.json`).
pub(crate) fn is_asset(specifier: &str) -> bool {
    specifier
        .rsplit('/')
        .next()
        .and_then(|last| last.rsplit_once('.'))
        .is_some_and(|(_, extension)| ASSET_EXTENSIONS.contains(&extension))
}

/// Splits `@scope/name/sub` or `name/sub` into the package name and optional subpath.
pub(crate) fn split_package(specifier: &str) -> (&str, Option<&str>) {
    let mut slashes = specifier.match_indices('/');
    let cut = if specifier.starts_with('@') {
        slashes.nth(1)
    } else {
        slashes.next()
    };
    match cut {
        Some((index, _)) => (&specifier[..index], Some(&specifier[index + 1..])),
        None => (specifier, None),
    }
}
