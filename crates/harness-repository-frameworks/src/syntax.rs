//! Small helpers over tree-sitter shared by the adapters: parsing by file extension, literal
//! text, spans, evidence, and relative path resolution. Nothing here knows any framework.
use std::path::{Component, Path, PathBuf};

use harness_workspace::{
    framework_relations::RelationEvidence, import_graph::ByteSpan,
    repository_intelligence::RootRelativePath,
};
use tree_sitter::{Language, Node, Parser, Tree};

fn extension(path: &RootRelativePath) -> Option<&str> {
    path.as_path().extension().and_then(|e| e.to_str())
}

/// Whether the file is TypeScript or JavaScript source the import index covers.
pub fn is_script(path: &RootRelativePath) -> bool {
    matches!(
        extension(path),
        Some("ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs")
    )
}

/// Parses TypeScript or JavaScript text with the grammar for `path`'s extension.
pub fn parse_script(path: &RootRelativePath, text: &str) -> Option<Tree> {
    let language: Language = match extension(path)? {
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "ts" | "mts" | "cts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        _ => tree_sitter_javascript::LANGUAGE.into(),
    };
    parse(&language, text)
}

/// Parses TypeScript text (a `.vue` script block, for example) regardless of any path.
pub fn parse_typescript(text: &str) -> Option<Tree> {
    parse(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), text)
}

pub fn parse_rust(text: &str) -> Option<Tree> {
    parse(&tree_sitter_rust::LANGUAGE.into(), text)
}

fn parse(language: &Language, text: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(language).ok()?;
    parser.parse(text, None)
}

pub fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

pub fn span(node: Node<'_>) -> ByteSpan {
    ByteSpan {
        start: node.start_byte(),
        end: node.end_byte(),
    }
}

pub fn evidence(file: &RootRelativePath, node: Node<'_>, source: &str) -> RelationEvidence {
    RelationEvidence::new(file, span(node), text(node, source))
}

/// Visits every node under `root` in source order.
pub fn walk<'t>(root: Node<'t>, visit: &mut impl FnMut(Node<'t>)) {
    let mut cursor = root.walk();
    let mut descend = true;
    loop {
        if descend {
            visit(cursor.node());
        }
        if descend && cursor.goto_first_child() {
            continue;
        }
        if cursor.goto_next_sibling() {
            descend = true;
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                return;
            }
            if cursor.node() == root {
                return;
            }
            if cursor.goto_next_sibling() {
                descend = true;
                break;
            }
        }
    }
}

/// The text of a plain string or an interpolation-free template literal, without its quotes.
pub fn string_value(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "string" | "string_literal" => {
            let raw = text(node, source);
            let inner = raw.get(1..raw.len().checked_sub(1)?)?;
            Some(inner.to_owned())
        }
        "template_string" => {
            let raw = text(node, source);
            if raw.contains("${") {
                return None;
            }
            Some(raw.get(1..raw.len().checked_sub(1)?)?.to_owned())
        }
        _ => None,
    }
}

/// `dir/../a/./b` collapsed, with `..` never leaving the root.
pub fn normalize(path: &Path) -> Option<PathBuf> {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_owned()),
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.iter().collect())
}

/// A relative specifier (`./x`, `../y/z`) taken from `file`, as a root-relative path.
pub fn join_relative(file: &RootRelativePath, specifier: &str) -> Option<RootRelativePath> {
    if !specifier.starts_with('.') {
        return None;
    }
    let joined = file
        .as_path()
        .parent()
        .unwrap_or(Path::new(""))
        .join(specifier);
    RootRelativePath::new(normalize(&joined)?).ok()
}

/// The first of `candidates` (exact, then with each extension, then `index` files) that exists in `known`.
pub fn find_module<'a>(
    base: &RootRelativePath,
    known: impl Fn(&RootRelativePath) -> bool + 'a,
    extensions: &[&str],
) -> Option<RootRelativePath> {
    if known(base) {
        return Some(base.clone());
    }
    for extension in extensions {
        let with =
            RootRelativePath::new(format!("{}.{extension}", base.as_path().display())).ok()?;
        if known(&with) {
            return Some(with);
        }
    }
    for extension in extensions {
        let index =
            RootRelativePath::new(base.as_path().join(format!("index.{extension}"))).ok()?;
        if known(&index) {
            return Some(index);
        }
    }
    None
}

/// Evidence for the last call of a method chain: `.route("/x", handler)` rather than the whole
/// chain of calls that leads up to it. Falls back to the whole call for other shapes.
pub fn method_call_evidence(
    file: &RootRelativePath,
    call: Node<'_>,
    source: &str,
) -> RelationEvidence {
    let start = call
        .child_by_field_name("function")
        .filter(|f| f.kind() == "field_expression")
        .and_then(|f| f.child_by_field_name("field"))
        .and_then(|field| source[..field.start_byte()].rfind('.'))
        .unwrap_or(call.start_byte());
    RelationEvidence::new(
        file,
        ByteSpan {
            start,
            end: call.end_byte(),
        },
        &source[start..call.end_byte()],
    )
}

pub fn parse_python(text: &str) -> Option<Tree> {
    parse(&tree_sitter_python::LANGUAGE.into(), text)
}

pub fn parse_go(text: &str) -> Option<Tree> {
    parse(&tree_sitter_go::LANGUAGE.into(), text)
}

pub fn parse_java(text: &str) -> Option<Tree> {
    parse(&tree_sitter_java::LANGUAGE.into(), text)
}

pub fn parse_csharp(text: &str) -> Option<Tree> {
    parse(&tree_sitter_c_sharp::LANGUAGE.into(), text)
}

pub fn parse_php(text: &str) -> Option<Tree> {
    parse(&tree_sitter_php::LANGUAGE_PHP.into(), text)
}
