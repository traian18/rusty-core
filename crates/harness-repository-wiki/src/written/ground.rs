//! Keeps a written page honest about what it names. Every backticked span that looks like a file
//! path or a code symbol must be something the writer was shown; one that is not is turned into
//! plain text (its sentence stays) and counted. Prose is left alone, and so are commands,
//! single lowercase words and fenced code. Diagram markers survive only for diagrams the page has.
use std::collections::BTreeSet;

pub const DIAGRAM_OPEN: &str = "@@diagram:";
pub const DIAGRAM_CLOSE: &str = "@@";

const SOURCE_EXTENSIONS: [&str; 26] = [
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "py", "go", "java", "kt", "kts",
    "cs", "php", "c", "h", "cpp", "hpp", "rb", "json", "toml", "md", "yaml", "yml",
];

pub struct Grounded {
    pub markdown: String,
    /// Paths named, resolved to repository paths, in order of first mention.
    pub cited: Vec<String>,
    /// References demoted to plain text.
    pub demoted: usize,
    pub diagrams: Vec<String>,
    /// Words of prose outside code blocks.
    pub words: usize,
}

pub struct Known<'a> {
    /// Every file of the repository, as repository-relative paths.
    pub paths: &'a [String],
    /// Everything the writer was shown, searched for symbols.
    pub evidence: &'a str,
}

fn looks_like_path(token: &str) -> bool {
    !token.contains(' ')
        && (token.contains('/')
            || token
                .rsplit_once('.')
                .is_some_and(|(stem, ext)| !stem.is_empty() && SOURCE_EXTENSIONS.contains(&ext)))
}

fn looks_like_symbol(token: &str) -> bool {
    let core = token.trim_end_matches("()");
    if core.is_empty()
        || core.contains(' ')
        || !core
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        return false;
    }
    if !core
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.'))
    {
        return false;
    }
    core.contains("::")
        || core.contains('.')
        || core.contains('_')
        || core.chars().skip(1).any(|c| c.is_ascii_uppercase())
}

impl Known<'_> {
    /// The repository path a backticked path means: exact, a unique suffix, or a folder.
    fn resolve(&self, token: &str) -> Option<String> {
        let token = token.trim_start_matches("./").trim_end_matches('/');
        if self.paths.iter().any(|p| p == token) {
            return Some(token.to_owned());
        }
        let suffix = format!("/{token}");
        let matches: Vec<&String> = self.paths.iter().filter(|p| p.ends_with(&suffix)).collect();
        if matches.len() == 1 {
            return Some(matches[0].clone());
        }
        let folder = format!("{token}/");
        if self
            .paths
            .iter()
            .any(|p| p.starts_with(&folder) || p.contains(&format!("/{folder}")))
        {
            return Some(String::new());
        }
        None
    }

    fn knows_symbol(&self, token: &str) -> bool {
        let core = token.trim_end_matches("()");
        let last = core.rsplit([':', '.']).next().unwrap_or(core);
        self.evidence.contains(core) || (!last.is_empty() && self.evidence.contains(last))
    }
}

pub fn ground(markdown: &str, known: &Known<'_>, diagram_keys: &[String]) -> Grounded {
    let mut out = String::new();
    let mut cited: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut diagrams = Vec::new();
    let mut demoted = 0;
    let mut words = 0;
    let mut fenced = false;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            fenced = !fenced;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if fenced {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if let Some(key) = trimmed
            .strip_prefix(DIAGRAM_OPEN)
            .and_then(|r| r.strip_suffix(DIAGRAM_CLOSE))
        {
            if diagram_keys.iter().any(|k| k == key) && !diagrams.iter().any(|d: &String| d == key)
            {
                diagrams.push(key.to_owned());
                out.push_str(trimmed);
                out.push('\n');
            }
            continue;
        }
        words += line.split_whitespace().count();
        let mut rest = line;
        while let Some(open) = rest.find('`') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let Some(close) = after.find('`') else {
                out.push_str(&rest[open..]);
                rest = "";
                break;
            };
            let token = &after[..close];
            let keep = if looks_like_path(token) {
                match known.resolve(token) {
                    Some(path) => {
                        if !path.is_empty() && seen.insert(path.clone()) {
                            cited.push(path);
                        }
                        true
                    }
                    None => false,
                }
            } else if looks_like_symbol(token) {
                known.knows_symbol(token)
            } else {
                true
            };
            if keep {
                out.push('`');
                out.push_str(token);
                out.push('`');
            } else {
                demoted += 1;
                out.push_str(token);
            }
            rest = &after[close + 1..];
        }
        out.push_str(rest);
        out.push('\n');
    }
    Grounded {
        markdown: out.trim_end().to_owned() + "\n",
        cited,
        demoted,
        diagrams,
        words,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known<'a>(paths: &'a [String], evidence: &'a str) -> Known<'a> {
        Known { paths, evidence }
    }

    #[test]
    fn real_paths_and_symbols_stay_invented_ones_become_text() {
        let paths = vec!["src/engine/run.rs".to_owned(), "web/app.ts".to_owned()];
        let evidence = "pub struct SessionEngine { } fn start_run() {}";
        let page = "## How it runs\nThe `SessionEngine` in `src/engine/run.rs` calls `start_run()`, then `NonExistent::thing` and `src/missing.rs`.\nRun `npm run dev` from `web/` or read `run.rs`.\n```\n`FakeThing` in code stays\n```\n";
        let g = ground(page, &known(&paths, evidence), &[]);
        assert!(g
            .markdown
            .contains("`SessionEngine` in `src/engine/run.rs` calls `start_run()`"));
        assert!(g
            .markdown
            .contains("then NonExistent::thing and src/missing.rs."));
        assert!(
            g.markdown
                .contains("`npm run dev` from `web/` or read `run.rs`"),
            "commands, folders and unique suffixes stay"
        );
        assert!(g.markdown.contains("`FakeThing` in code stays"));
        assert_eq!(g.demoted, 2);
        assert_eq!(g.cited, ["src/engine/run.rs"]);
    }

    #[test]
    fn diagram_markers_survive_only_for_known_diagrams() {
        let g = ground(
            "intro\n@@diagram:components@@\n@@diagram:made_up@@\n@@diagram:components@@\n",
            &known(&[], ""),
            &["components".to_owned()],
        );
        assert_eq!(g.diagrams, ["components"]);
        assert_eq!(g.markdown.matches(DIAGRAM_OPEN).count(), 1);
    }
}
