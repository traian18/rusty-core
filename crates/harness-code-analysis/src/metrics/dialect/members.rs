//! Members read without a receiver: in Java and Kotlin a method names its fields and sibling
//! methods directly (`count++`, `helper()`), so cohesion needs those as well as `this.x`.
//!
//! A bare name is a member when the nearest enclosing class body declares it and no parameter or
//! local between the use and that body shadows it. Locals are matched by name in the enclosing
//! blocks, without regard to declaration order.
use tree_sitter::Node;

use super::text;

/// What one grammar calls the pieces the lookup needs.
pub struct Grammar {
    /// Nodes that hold a class's member declarations.
    pub class_bodies: &'static [&'static str],
    /// Whether the identifier is a use of a name, not a declaration or a qualified part.
    pub is_reference: fn(Node<'_>) -> bool,
    /// Names that `scope` itself declares for its descendants: parameters, locals of a block.
    pub scope_names: fn(Node<'_>, &[u8], &mut Vec<String>),
    /// Fields and methods declared by a class body (and its header, such as constructor
    /// properties).
    pub member_names: fn(Node<'_>, &[u8], &mut Vec<String>),
}

pub fn implicit_member(node: Node<'_>, source: &[u8], grammar: &Grammar) -> Option<String> {
    if node.kind() != "identifier" || !(grammar.is_reference)(node) {
        return None;
    }
    let name = text(node, source);
    let mut names = Vec::new();
    let mut current = node.parent();
    while let Some(scope) = current {
        if grammar.class_bodies.contains(&scope.kind()) {
            names.clear();
            (grammar.member_names)(scope, source, &mut names);
            return names.iter().any(|n| n == name).then(|| name.to_owned());
        }
        names.clear();
        (grammar.scope_names)(scope, source, &mut names);
        if names.iter().any(|n| n == name) {
            return None;
        }
        current = scope.parent();
    }
    None
}

/// Named children of `node` with the given kind.
pub fn children_of<'t>(node: Node<'t>, kind: &str) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|n| n.kind() == kind)
        .collect()
}
