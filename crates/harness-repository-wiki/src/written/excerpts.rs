//! Real code for the writer to read: the central classes and functions of a file, measured with
//! the code-analysis metrics, each with the comments above it.
use harness_code_analysis::{metrics::measure_file, LanguageId};

use super::evidence::Evidence;

/// Lines of comment or attribute directly above a symbol that belong to it.
const LEADING: usize = 8;

pub fn language_of(path: &str) -> Option<LanguageId> {
    let ext = path.rsplit('.').next()?;
    match ext {
        "rs" => Some(LanguageId::Rust),
        "ts" | "tsx" | "mts" | "cts" => Some(LanguageId::TypeScript),
        "js" | "jsx" | "mjs" | "cjs" => Some(LanguageId::JavaScript),
        "py" => Some(LanguageId::Python),
        "java" => Some(LanguageId::Java),
        "kt" | "kts" => Some(LanguageId::Kotlin),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub class: bool,
    pub start: u32,
    pub end: u32,
    weight: u64,
}

fn is_leading(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("///")
        || t.starts_with("//!")
        || t.starts_with("//")
        || t.starts_with("/**")
        || t.starts_with('*')
        || t.starts_with("#[")
        || t.starts_with('@')
        || (t.starts_with('#') && !t.starts_with("#!"))
}

/// The classes (and `impl` / trait blocks) and top-level functions of a file, largest first.
pub fn symbols(path: &str, text: &str) -> Vec<Symbol> {
    let Some(language) = language_of(path) else {
        return Vec::new();
    };
    let Ok(metrics) = measure_file(&language, path, text.as_bytes()) else {
        return Vec::new();
    };
    let mut out: Vec<Symbol> = metrics
        .classes
        .iter()
        .map(|c| Symbol {
            name: c.name.clone(),
            class: true,
            start: c.start_line,
            end: c.end_line,
            weight: u64::from(c.nloc) * 4 + u64::from(c.method_count) * 20,
        })
        .collect();
    out.extend(
        metrics
            .functions
            .iter()
            .filter(|f| {
                f.class.is_none()
                    && f.kind == harness_code_analysis::metrics::FunctionKind::Function
            })
            .map(|f| Symbol {
                name: f.name.clone(),
                class: false,
                start: f.start_line,
                end: f.end_line,
                weight: u64::from(f.nloc) * u64::from(f.ccn),
            }),
    );
    out.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.start.cmp(&b.start)));
    out
}

/// Adds up to `count` central symbols of `path` as excerpts of at most `max_lines` lines each.
pub fn add(evidence: &mut Evidence<'_>, path: &str, count: usize, max_lines: u32) -> Vec<Symbol> {
    let Some(text) = evidence.read(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let chosen: Vec<Symbol> = symbols(path, &text).into_iter().take(count).collect();
    for symbol in &chosen {
        let mut start = symbol.start as usize;
        while start > 1
            && symbol.start as usize - start < LEADING
            && lines.get(start - 2).is_some_and(|l| is_leading(l))
        {
            start -= 1;
        }
        let label = format!(
            "{} `{}`",
            if symbol.class { "type" } else { "function" },
            symbol.name
        );
        evidence.lines(path, start as u32, symbol.end, max_lines, &label);
        evidence.mention(&symbol.name);
    }
    if chosen.is_empty() {
        // Nothing measurable: the start of the file says what it is.
        evidence.lines(path, 1, 30, 30, "start of file");
    }
    chosen
}
