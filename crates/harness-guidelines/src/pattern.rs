//! Pattern rules: a regex over each line, or a tree-sitter query over the syntax.
use harness_code_analysis::{metrics, LanguageId};
use regex::Regex;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

pub(crate) enum Pattern {
    Regex(Regex),
    /// Compiled per file against its grammar (TypeScript and TSX differ); checked once at load.
    Query {
        source: String,
        language: LanguageId,
    },
}

/// A line range a pattern matched, 1-based and inclusive.
pub(crate) struct Hit {
    pub start_line: u32,
    pub end_line: u32,
}

pub(crate) fn language_named(name: &str) -> Option<(LanguageId, &'static str)> {
    Some(match name.to_ascii_lowercase().as_str() {
        "rust" | "rs" => (LanguageId::Rust, "x.rs"),
        "typescript" | "ts" => (LanguageId::TypeScript, "x.ts"),
        "tsx" => (LanguageId::TypeScript, "x.tsx"),
        "javascript" | "js" | "jsx" => (LanguageId::JavaScript, "x.js"),
        "python" | "py" => (LanguageId::Python, "x.py"),
        "java" => (LanguageId::Java, "x.java"),
        "kotlin" | "kt" => (LanguageId::Kotlin, "x.kt"),
        "csharp" | "cs" | "c#" => (LanguageId::CSharp, "x.cs"),
        "go" => (LanguageId::Go, "x.go"),
        "php" => (LanguageId::Php, "x.php"),
        "cpp" | "c++" | "c" => (LanguageId::Cpp, "x.cpp"),
        "ruby" | "rb" => (LanguageId::Ruby, "x.rb"),
        _ => return None,
    })
}

pub(crate) fn compile(
    regex: Option<&str>,
    query: Option<&str>,
    lang: Option<&str>,
) -> Result<Pattern, String> {
    match (regex, query) {
        (Some(regex), None) => Regex::new(regex)
            .map(Pattern::Regex)
            .map_err(|e| format!("the regex does not compile: {e}")),
        (None, Some(query)) => {
            let lang = lang.ok_or("a query needs 'lang' (rust, typescript, python, ...)")?;
            let (language, sample) =
                language_named(lang).ok_or_else(|| format!("'{lang}' is not a known language"))?;
            let grammar = metrics::grammar(&language, sample)
                .ok_or_else(|| format!("'{lang}' has no grammar"))?;
            Query::new(&grammar, query).map_err(|e| format!("the query does not compile: {e}"))?;
            Ok(Pattern::Query {
                source: query.to_owned(),
                language,
            })
        }
        (Some(_), Some(_)) => Err("set either 'regex' or 'query', not both".into()),
        (None, None) => Err("a pattern rule needs 'regex' or 'query'".into()),
    }
}

impl Pattern {
    /// Where the pattern matches `source` (the file at `path`).
    pub fn hits(&self, path: &str, source: &str) -> Vec<Hit> {
        match self {
            Self::Regex(regex) => source
                .lines()
                .enumerate()
                .filter(|(_, line)| regex.is_match(line))
                .map(|(i, _)| Hit {
                    start_line: i as u32 + 1,
                    end_line: i as u32 + 1,
                })
                .collect(),
            Self::Query {
                source: query,
                language,
            } => query_hits(query, language, path, source),
        }
    }
}

fn query_hits(query: &str, language: &LanguageId, path: &str, source: &str) -> Vec<Hit> {
    // A query for one language says nothing about files in another.
    if metrics::language_of(path).as_ref() != Some(language) {
        return Vec::new();
    }
    let Some(grammar) = metrics::grammar(language, path) else {
        return Vec::new();
    };
    let Ok(query) = Query::new(&grammar, query) else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&grammar).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let marked = query.capture_names().iter().position(|n| *n == "violation");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut hits = Vec::new();
    while let Some(found) = matches.next() {
        let capture = match marked {
            Some(index) => found.captures.iter().find(|c| c.index as usize == index),
            None => found.captures.first(),
        };
        if let Some(capture) = capture {
            hits.push(Hit {
                start_line: capture.node.start_position().row as u32 + 1,
                end_line: capture.node.end_position().row as u32 + 1,
            });
        }
    }
    hits.dedup_by(|a, b| a.start_line == b.start_line && a.end_line == b.end_line);
    hits
}
