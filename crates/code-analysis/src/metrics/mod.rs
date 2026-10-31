//! Size and complexity of every function and class in a file, from one tree-sitter parse.
//!
//! These are the measurements code health is computed from: lines of code, cyclomatic and
//! cognitive complexity, nesting, "bumps" and parameters per function, and size and cohesion
//! (LCOM4) per class. They are facts about the code as written; thresholds and judgements belong
//! to the code health layer that reads them.
//!
//! Definitions (the same for every language):
//!
//! - **NLOC**: lines in the span that hold at least one token outside a comment. Docstrings
//!   (a string standing alone as the first statement of a Python module, class or function) count
//!   as comments. A function's span includes any functions nested in it.
//! - **CCN** (McCabe): 1, plus 1 for each `if` / `else if` / `elif`, loop, `catch` / `except`,
//!   non-default `case` or match arm, conditional expression, comprehension `if`, and each `&&` /
//!   `||` / `and` / `or`.
//! - **Cognitive** (after SonarSource): `if`, loops, `switch` / `match` and `catch` add 1 plus
//!   the current nesting; `else if`, `else`, conditional expressions and comprehension `if` add 1;
//!   each run of the same boolean operator adds 1.
//! - **Max nesting**: the deepest control block (`if`, loop, `switch` / `match`, `catch`); a body
//!   directly in the function is depth 1, and `else if` chains do not add depth.
//! - **Bumps**: control statements directly in the function body that contain another control
//!   block (nesting of 2 or more), each one a separate "bump" of nested logic.
//! - **Params**: declared parameters, without the receiver (`self`, `&self`, Python's first
//!   parameter of a method, TypeScript's `this:`), separators (`*`, `/`) included as nothing.
//!
//! Every function-like node is measured on its own, including closures, lambdas and arrow
//! functions; its complexity never includes the functions nested inside it.
mod class;
mod dialect;
mod extras;
mod lines;
mod model;
mod smells;
mod walk;

pub use model::{
    ClassMetrics, Condition, ErrorStats, FileMetrics, FunctionKind, FunctionMetrics, MetricsError,
};

use crate::model::LanguageId;

/// Bumped when what a measurement holds changes, so saved measurements are not reused.
pub const METRICS_VERSION: u32 = 3;

/// Languages measured today.
pub fn metrics_supported(language: &LanguageId) -> bool {
    dialect::for_language(language).is_some()
}

/// The tree-sitter grammar `path` is parsed with, for queries over its syntax; `None` for a
/// language that is not measured.
pub fn grammar(language: &LanguageId, path: &str) -> Option<tree_sitter::Language> {
    dialect::for_language(language).map(|dialect| dialect.grammar(path))
}

/// The measured language of a file, by its extension.
pub fn language_of(path: &str) -> Option<LanguageId> {
    crate::adapters::language_for(path)
}

/// Measures every function and class in `source`. A file with syntax errors is still measured
/// and marked `partial`.
pub fn measure_file(
    language: &LanguageId,
    path: &str,
    source: &[u8],
) -> Result<FileMetrics, MetricsError> {
    let dialect = dialect::for_language(language)
        .ok_or_else(|| MetricsError::Unsupported(format!("{language:?}")))?;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&dialect.grammar(path))
        .map_err(|e| MetricsError::Parse(e.to_string()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| MetricsError::Parse("the parser returned no tree".into()))?;
    let root = tree.root_node();
    let lines = lines::CodeLines::new(root, source, dialect.as_ref());
    let mut functions = Vec::new();
    let mut classes = Vec::new();
    let env = walk::Env {
        language,
        source,
        dialect: dialect.as_ref(),
        lines: &lines,
    };
    walk::collect(&env, root, None, &mut functions, &mut classes);
    for class in &mut classes {
        class::finish(class, &functions);
    }
    delegate_assertions(&mut functions, source);
    Ok(FileMetrics {
        language: language.clone(),
        nloc: lines.total(),
        partial: root.has_error(),
        functions,
        classes: classes.into_iter().map(|c| c.metrics).collect(),
        skipped_tests: smells::count_skipped(&String::from_utf8_lossy(source)),
    })
}

/// A test that calls a helper which asserts (`attempt(...)`, `check_ok(...)`) asserts through it.
fn delegate_assertions(functions: &mut [FunctionMetrics], source: &[u8]) {
    let helpers: Vec<String> = functions
        .iter()
        .filter(|f| !f.test && f.assertions > 0 && !f.name.starts_with('<'))
        .map(|f| format!("{}(", f.name))
        .collect();
    if helpers.is_empty() || functions.iter().all(|f| !f.test || f.assertions > 0) {
        return;
    }
    let text = String::from_utf8_lossy(source);
    let lines: Vec<&str> = text.lines().collect();
    for f in functions.iter_mut().filter(|f| f.test && f.assertions == 0) {
        let body = lines
            .get(f.start_line as usize - 1..(f.end_line as usize).min(lines.len()))
            .unwrap_or(&[])
            .join("\n");
        if helpers.iter().any(|h| body.contains(h.as_str())) {
            f.assertions = 1;
        }
    }
}
