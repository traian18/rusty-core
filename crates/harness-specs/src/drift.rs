//! Specs describe the system as it is, so a capability or the project file that names a file which
//! is not there has drifted from the code. Deterministic: backticked paths are looked up on disk.
use crate::{
    model::{Severity, Warning},
    store::SpecStore,
};

/// File extensions that make a backticked word without a folder look like a path worth checking.
/// Only words with a `/` are checked at all, so `foo.rs` alone (which could be anywhere) is not.
const MAX_PATH_CHARS: usize = 200;

/// Notes for every path `text` (the document at `file`) names that does not exist in the repository.
pub fn missing_paths(store: &dyn SpecStore, file: &str, text: &str) -> Vec<Warning> {
    let mut out = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut fenced = false;
    for (index, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        if fenced {
            continue;
        }
        for token in backticked(line) {
            let Some(path) = as_path(token) else { continue };
            if seen.contains(&path) || store.exists(&path) {
                continue;
            }
            out.push(Warning::new(
                file,
                Some(index + 1),
                Severity::Note,
                format!("`{path}` is named here but does not exist in the repository"),
            ));
            seen.push(path);
        }
    }
    out
}

fn backticked(line: &str) -> Vec<&str> {
    line.split('`')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, token)| token)
        .collect()
}

/// The repository path a backticked word stands for: it has a folder, no spaces, no globs or
/// placeholders, is not a URL, and loses a trailing `:line` or `::Symbol`.
fn as_path(token: &str) -> Option<String> {
    let token = token.trim().trim_end_matches('/');
    let token = token.split("::").next().unwrap_or(token);
    let token = token
        .rsplit_once(':')
        .filter(|(_, tail)| tail.chars().all(|c| c.is_ascii_digit() || c == '-'))
        .map_or(token, |(head, _)| head);
    let plain = !token.is_empty()
        && token.len() <= MAX_PATH_CHARS
        && token.contains('/')
        && !token.starts_with('/')
        && !token.starts_with("./")
        && !token.starts_with("../")
        && !token.contains("//")
        && !token.chars().any(|c| {
            c.is_whitespace()
                || matches!(
                    c,
                    '*' | '<'
                        | '>'
                        | '{'
                        | '}'
                        | '?'
                        | '$'
                        | '\\'
                        | '|'
                        | ','
                        | '('
                        | ')'
                        | '"'
                        | '\''
                        | '~'
                )
        })
        && !token.starts_with('@')
        && token
            .split('/')
            .all(|part| !part.is_empty() && part != "..");
    plain.then(|| token.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;

    fn repo() -> MemoryStore {
        let store = MemoryStore::default();
        store.insert("src/albums/model.rs", "x");
        store.insert("docs/guide.md", "x");
        store
    }

    #[test]
    fn a_named_path_that_is_missing_is_reported_with_its_line() {
        let text = "# Albums\n\nStored in `src/albums/model.rs` and `src/albums/store.rs`.\nSee `docs/guide.md`.\n";
        let found = missing_paths(&repo(), "specs/capabilities/albums/spec.md", text);
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].line, found[0].severity),
            (Some(3), Severity::Note)
        );
        assert!(found[0]
            .message
            .starts_with("`src/albums/store.rs` is named here"));
    }

    #[test]
    fn folders_and_line_or_symbol_suffixes_are_looked_up_as_paths() {
        let text = "`src/albums/` and `src/albums/model.rs:12-20` and `src/albums/model.rs::Album` and `src/gone/`.";
        let found = missing_paths(&repo(), "f", text);
        assert_eq!(found.len(), 1);
        assert!(found[0].message.contains("`src/gone`"));
    }

    #[test]
    fn words_that_are_not_paths_are_left_alone() {
        let text = "`foo.rs` `a b/c` `src/*.rs` `<dir>/x` `https://example.com/x` `/abs/x` `./x/y` `@scope/pkg` `WHEN/THEN` `x/{a,b}`";
        let found = missing_paths(&repo(), "f", text);
        // `WHEN/THEN` has a folder and no spaces, so it is looked up; the rest are not.
        assert_eq!(found.len(), 1);
        assert!(found[0].message.contains("`WHEN/THEN`"));
    }

    #[test]
    fn code_blocks_are_skipped_and_a_path_is_reported_once() {
        let text = "```\n`src/a/b.rs`\n```\n`src/c/d.rs` and `src/c/d.rs` again\n";
        let found = missing_paths(&repo(), "f", text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, Some(4));
    }
}
