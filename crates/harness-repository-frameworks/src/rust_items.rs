//! Functions and their attributes in Rust source, shared by the Rust framework adapters.
use std::collections::HashSet;

use harness_workspace::{
    framework_relations::FrameworkInput, repository_intelligence::RootRelativePath,
};
use tree_sitter::Node;

use crate::syntax::{string_value, text, walk};

/// `#[path(args)]`.
pub struct Attribute<'t> {
    /// The attribute path as written, for example `tauri::command` or `get`.
    pub path: String,
    pub arguments: Option<Node<'t>>,
    pub node: Node<'t>,
}

pub struct Function<'t> {
    pub name: String,
    pub attributes: Vec<Attribute<'t>>,
}

impl Function<'_> {
    pub fn attribute(&self, paths: &[&str]) -> Option<&Attribute<'_>> {
        self.attributes
            .iter()
            .find(|a| paths.contains(&a.path.as_str()))
    }
}

fn attribute<'t>(item: Node<'t>, source: &str) -> Option<Attribute<'t>> {
    let inner = item.named_child(0).filter(|n| n.kind() == "attribute")?;
    let path = inner.named_child(0)?;
    Some(Attribute {
        path: text(path, source).replace(' ', ""),
        arguments: inner.child_by_field_name("arguments"),
        node: item,
    })
}

/// Every function item with the attributes written before it.
pub fn functions<'t>(root: Node<'t>, source: &str) -> Vec<Function<'t>> {
    let mut found = Vec::new();
    walk(root, &mut |parent| {
        let mut pending = Vec::new();
        let mut cursor = parent.walk();
        for child in parent.named_children(&mut cursor) {
            match child.kind() {
                "attribute_item" => pending.extend(attribute(child, source)),
                "function_item" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        found.push(Function {
                            name: text(name, source).to_owned(),
                            attributes: std::mem::take(&mut pending),
                        });
                    }
                    pending.clear();
                }
                "line_comment" | "block_comment" => {}
                _ => pending.clear(),
            }
        }
    });
    found
}

/// The first string literal inside an attribute's or macro's token tree.
pub fn first_string(tokens: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = tokens.walk();
    let found = tokens
        .named_children(&mut cursor)
        .find(|n| n.kind() == "string_literal")
        .and_then(|n| string_value(n, source));
    found
}

/// Names of functions declared in the file, for resolving a bare handler name.
pub fn local_functions(root: Node<'_>, source: &str) -> HashSet<String> {
    functions(root, source)
        .into_iter()
        .map(|f| f.name)
        .collect()
}

/// The file a handler path names: a function in this file, a function brought in with `use`, or
/// `module::function` where `module` resolves to a file. `None` for anything else.
pub fn resolve_path(
    node: Node<'_>,
    file: &RootRelativePath,
    source: &str,
    locals: &HashSet<String>,
    input: &FrameworkInput<'_>,
) -> (Option<RootRelativePath>, Option<String>) {
    match node.kind() {
        "identifier" => {
            let name = text(node, source);
            let target = if locals.contains(name) {
                Some(file.clone())
            } else {
                input.names.target(file, name).cloned()
            };
            (target, Some(name.to_owned()))
        }
        "scoped_identifier" => {
            let full = text(node, source).replace(' ', "");
            let segments: Vec<&str> = full.split("::").collect();
            let Some(last) = segments.last() else {
                return (None, None);
            };
            // `module::function`: the module name was imported or declared in this file.
            let target = segments
                .first()
                .and_then(|first| {
                    if matches!(*first, "crate" | "self" | "super") {
                        None
                    } else {
                        input.names.target(file, first).cloned()
                    }
                })
                .or_else(|| input.names.target(file, last).cloned());
            (target, Some((*last).to_owned()))
        }
        "closure_expression" => (Some(file.clone()), None),
        _ => (None, None),
    }
}
