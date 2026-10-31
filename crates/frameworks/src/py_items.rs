//! Functions, decorators and module-level assignments in Python source, shared by the Python
//! framework adapters.
use std::collections::HashMap;

use tree_sitter::Node;

use crate::syntax::{text, walk};

/// `@receiver.name(args, key=value)`; `receiver` is the dotted object before the last segment.
pub struct Decorator<'t> {
    pub receiver: Option<String>,
    pub name: String,
    pub args: Vec<Node<'t>>,
    pub kwargs: Vec<(String, Node<'t>)>,
    pub node: Node<'t>,
}

impl<'t> Decorator<'t> {
    pub fn kwarg(&self, name: &str) -> Option<Node<'t>> {
        self.kwargs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| *value)
    }
}

pub struct Function<'t> {
    pub name: String,
    pub decorators: Vec<Decorator<'t>>,
    pub parameters: Option<Node<'t>>,
}

/// Splits `a.b.c` into (`a.b`, `c`).
fn split_dotted(callee: &str) -> (Option<String>, String) {
    match callee.rsplit_once('.') {
        Some((receiver, name)) => (Some(receiver.to_owned()), name.to_owned()),
        None => (None, callee.to_owned()),
    }
}

/// The positional and keyword arguments of a call's `argument_list`.
pub fn call_arguments<'t>(
    call: Node<'t>,
    source: &str,
) -> (Vec<Node<'t>>, Vec<(String, Node<'t>)>) {
    let (mut args, mut kwargs) = (Vec::new(), Vec::new());
    if let Some(list) = call.child_by_field_name("arguments") {
        let mut cursor = list.walk();
        for child in list.named_children(&mut cursor) {
            if child.kind() == "keyword_argument" {
                if let (Some(name), Some(value)) = (
                    child.child_by_field_name("name"),
                    child.child_by_field_name("value"),
                ) {
                    kwargs.push((text(name, source).to_owned(), value));
                }
            } else if child.kind() != "comment" {
                args.push(child);
            }
        }
    }
    (args, kwargs)
}

fn decorator<'t>(node: Node<'t>, source: &str) -> Option<Decorator<'t>> {
    let inner = node.named_child(0)?;
    let (callee, args, kwargs) = match inner.kind() {
        "call" => {
            let function = inner.child_by_field_name("function")?;
            let (args, kwargs) = call_arguments(inner, source);
            (text(function, source).to_owned(), args, kwargs)
        }
        "attribute" | "identifier" => (text(inner, source).to_owned(), Vec::new(), Vec::new()),
        _ => return None,
    };
    let (receiver, name) = split_dotted(&callee);
    Some(Decorator {
        receiver,
        name,
        args,
        kwargs,
        node,
    })
}

/// Every function definition with the decorators written above it.
pub fn functions<'t>(root: Node<'t>, source: &str) -> Vec<Function<'t>> {
    let mut found = Vec::new();
    walk(root, &mut |node| {
        if node.kind() != "function_definition" {
            return;
        }
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let decorators = node
            .parent()
            .filter(|p| p.kind() == "decorated_definition")
            .map(|parent| {
                let mut cursor = parent.walk();
                parent
                    .children(&mut cursor)
                    .filter(|c| c.kind() == "decorator")
                    .filter_map(|c| decorator(c, source))
                    .collect()
            })
            .unwrap_or_default();
        found.push(Function {
            name: text(name, source).to_owned(),
            decorators,
            parameters: node.child_by_field_name("parameters"),
        });
    });
    found
}

/// Module-level `name = Callee(...)`: the callee's last segment and the call.
pub fn assignments<'t>(root: Node<'t>, source: &str) -> HashMap<String, (String, Node<'t>)> {
    let mut found = HashMap::new();
    let mut cursor = root.walk();
    for statement in root.children(&mut cursor) {
        let Some(assignment) = statement
            .named_child(0)
            .filter(|n| statement.kind() == "expression_statement" && n.kind() == "assignment")
        else {
            continue;
        };
        let (Some(left), Some(right)) = (
            assignment.child_by_field_name("left"),
            assignment.child_by_field_name("right"),
        ) else {
            continue;
        };
        if left.kind() != "identifier" || right.kind() != "call" {
            continue;
        }
        let Some(function) = right.child_by_field_name("function") else {
            continue;
        };
        let (_, callee) = split_dotted(text(function, source));
        found.insert(text(left, source).to_owned(), (callee, right));
    }
    found
}

/// The text of a plain (non-f) string literal.
pub fn string_value(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let start = text(node, source)
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>();
    if start.to_lowercase().contains('f') {
        return None;
    }
    let mut cursor = node.walk();
    let mut out = String::new();
    for part in node.named_children(&mut cursor) {
        match part.kind() {
            "string_content" => out.push_str(text(part, source)),
            "interpolation" => return None,
            _ => {}
        }
    }
    Some(out)
}

/// Joins path parts with single slashes and a leading one.
pub fn join_path(prefix: &str, path: &str) -> String {
    let parts: Vec<&str> = [prefix, path]
        .iter()
        .flat_map(|p| p.split('/'))
        .filter(|p| !p.is_empty())
        .collect();
    let mut joined = format!("/{}", parts.join("/"));
    if path.ends_with('/') && !joined.ends_with('/') {
        joined.push('/');
    }
    joined
}
