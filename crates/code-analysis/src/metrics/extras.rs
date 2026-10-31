//! Per-function measurements that are not about control flow: failure handling, the shape of the
//! syntax tree for clone detection, and assertions in tests.
use tree_sitter::Node;

use super::{
    dialect::Dialect,
    model::ErrorStats,
    smells::{count_assertions, error_smell, in_test_code, is_test, ErrorSmell},
};
use crate::model::LanguageId;

/// Functions with fewer nodes than this are not compared for duplication.
const MIN_SHAPE_NODES: u32 = 12;

pub struct Extras {
    pub test: bool,
    pub test_code: bool,
    pub errors: ErrorStats,
    pub shape: u64,
    pub shape_nodes: u32,
    pub assertions: u32,
}

fn fnv(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

/// Hashes node kinds and depths in document order, skipping comments.
fn shape(node: Node<'_>, depth: u8, dialect: &dyn Dialect, hash: &mut u64, nodes: &mut u32) {
    if !node.is_named() || dialect.is_comment(node) {
        return;
    }
    fnv(hash, node.kind().as_bytes());
    fnv(hash, &[depth]);
    *nodes += 1;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        shape(child, depth.saturating_add(1), dialect, hash, nodes);
    }
}

fn errors(
    language: &LanguageId,
    node: Node<'_>,
    source: &[u8],
    dialect: &dyn Dialect,
    out: &mut ErrorStats,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // A nested function is measured on its own.
        if dialect.function_kind(child).is_some() {
            continue;
        }
        match error_smell(language, child, source) {
            Some(ErrorSmell::EmptyHandler) => out.empty_handlers += 1,
            Some(ErrorSmell::BroadHandler) => out.broad_handlers += 1,
            Some(ErrorSmell::Panic) => out.panics += 1,
            Some(ErrorSmell::IgnoredResult) => out.ignored_results += 1,
            None => {}
        }
        errors(language, child, source, dialect, out);
    }
}

/// The text of the function and of the attributes or decorators written above it.
fn with_attributes(function: Node<'_>, source: &[u8]) -> String {
    let mut start = function.start_byte();
    let mut previous = function.prev_sibling();
    while let Some(p) = previous.filter(|p| p.kind() == "attribute_item") {
        start = p.start_byte();
        previous = p.prev_sibling();
    }
    if let Some(parent) = function
        .parent()
        .filter(|p| p.kind() == "decorated_definition")
    {
        start = start.min(parent.start_byte());
    }
    String::from_utf8_lossy(&source[start..function.end_byte()]).into_owned()
}

pub fn scan(
    language: &LanguageId,
    function: Node<'_>,
    name: &str,
    source: &[u8],
    dialect: &dyn Dialect,
) -> Extras {
    let test = is_test(language, function, name, source);
    let test_code = test || in_test_code(language, function, name, source);
    let mut stats = ErrorStats::default();
    if !test_code {
        // Unwrapping and discarding results is how tests and their helpers are written.
        errors(language, function, source, dialect, &mut stats);
    }
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    fnv(&mut hash, format!("{language:?}").as_bytes());
    let mut nodes = 0;
    shape(function, 0, dialect, &mut hash, &mut nodes);
    let comparable = nodes >= MIN_SHAPE_NODES;
    Extras {
        test,
        test_code,
        errors: stats,
        shape: if comparable { hash } else { 0 },
        shape_nodes: nodes,
        assertions: count_assertions(&with_attributes(function, source)),
    }
}
