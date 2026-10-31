//! Route tables written as arrays of objects (`{ path: 'users', component: Users, children: […] }`),
//! as Angular's router and Vue Router both do.
use tree_sitter::Node;

use crate::syntax::{string_value, text, walk};

/// What a route entry points at.
pub enum Target<'t> {
    /// A component named by an identifier.
    Name(String),
    /// A component loaded lazily with `import('./specifier')`.
    Lazy(String),
    /// A component expression this reader does not follow.
    Other(Node<'t>),
}

pub struct Route<'t> {
    /// The path of every enclosing entry joined with this one's, with a leading slash.
    pub path: String,
    pub target: Target<'t>,
    pub node: Node<'t>,
}

pub struct RouteTable<'t> {
    pub routes: Vec<Route<'t>>,
    /// Entries skipped because a path was computed, as messages for diagnostics.
    pub skipped: Vec<String>,
}

pub(crate) fn pair_value<'t>(object: Node<'t>, key: &str, source: &str) -> Option<Node<'t>> {
    let mut cursor = object.walk();
    for pair in object.named_children(&mut cursor) {
        // `{ path }` is shorthand for `{ path: path }`: the value is the variable, so it is computed.
        if pair.kind() == "shorthand_property_identifier" && text(pair, source) == key {
            return Some(pair);
        }
        if pair.kind() == "pair" {
            let name = pair.child_by_field_name("key")?;
            if text(name, source).trim_matches(['\'', '"']) == key {
                return pair.child_by_field_name("value");
            }
        }
    }
    None
}

/// The `import('…')` specifier inside a lazy loader such as `() => import('./x').then(m => m.X)`.
fn lazy_specifier(node: Node<'_>, source: &str) -> Option<String> {
    let mut found = None;
    walk(node, &mut |n| {
        if found.is_some() || n.kind() != "call_expression" {
            return;
        }
        let Some(function) = n.child_by_field_name("function") else {
            return;
        };
        if text(function, source) == "import" {
            found = n
                .child_by_field_name("arguments")
                .and_then(|a| a.named_child(0))
                .and_then(|a| string_value(a, source));
        }
    });
    found
}

fn join(prefix: &str, part: &str) -> String {
    let parts: Vec<&str> = [prefix, part]
        .iter()
        .flat_map(|p| p.split('/'))
        .filter(|p| !p.is_empty())
        .collect();
    format!("/{}", parts.join("/"))
}

/// The enclosing route objects of `object`, outermost first, found through `children` arrays.
fn ancestors<'t>(object: Node<'t>) -> Vec<Node<'t>> {
    let mut chain = Vec::new();
    let mut current = object.parent();
    while let Some(node) = current {
        if node.kind() == "pair" {
            // pair -> object whose `children` holds this array
            if let Some(owner) = node.parent().filter(|p| p.kind() == "object") {
                chain.push(owner);
            }
        }
        current = node.parent();
    }
    chain.reverse();
    chain
}

/// Reads every route entry in `root`. `component_keys` are the properties that name a component
/// (`component`, `loadComponent`).
pub fn route_table<'t>(root: Node<'t>, source: &str, component_keys: &[&str]) -> RouteTable<'t> {
    let mut table = RouteTable {
        routes: Vec::new(),
        skipped: Vec::new(),
    };
    walk(root, &mut |node| {
        if node.kind() != "object" {
            return;
        }
        let Some(path_node) = pair_value(node, "path", source) else {
            return;
        };
        let Some(component) = component_keys
            .iter()
            .find_map(|key| pair_value(node, key, source))
        else {
            return;
        };
        let mut path = String::new();
        for owner in ancestors(node).into_iter().chain([node]) {
            let Some(part) = pair_value(owner, "path", source) else {
                continue;
            };
            match string_value(part, source) {
                Some(value) => path = join(&path, &value),
                None => {
                    table.skipped.push(format!(
                        "a route with computed path `{}` was not read",
                        text(part, source)
                    ));
                    return;
                }
            }
        }
        let _ = path_node;
        let target = match component.kind() {
            "identifier" => Target::Name(text(component, source).to_owned()),
            _ => match lazy_specifier(component, source) {
                Some(specifier) => Target::Lazy(specifier),
                None => Target::Other(component),
            },
        };
        table.routes.push(Route {
            path: if path.is_empty() { "/".into() } else { path },
            target,
            node,
        });
    });
    table
}
