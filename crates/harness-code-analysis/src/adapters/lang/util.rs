//! Helpers shared by the language modules.

use tree_sitter::Node;

use crate::adapters::collector::{CallSpec, Collector};
use crate::adapters::facts::{CallKind, Receiver};

/// Segments of a dotted/scoped name expression, or `None` for anything else.
pub(super) fn path_segments(node: Node<'_>, src: &str) -> Option<Vec<String>> {
    let text = |n: Node<'_>| src[n.byte_range()].to_owned();
    let join = |object: Option<Node<'_>>, name: Option<Node<'_>>| -> Option<Vec<String>> {
        let mut segments = path_segments(object?, src)?;
        segments.extend(path_segments(name?, src)?);
        Some(segments)
    };
    match node.kind() {
        "identifier"
        | "constant"
        | "type_identifier"
        | "namespace_identifier"
        | "package_identifier"
        | "field_identifier"
        | "property_identifier"
        | "name"
        | "private_property_identifier"
        | "simple_identifier" => Some(vec![text(node)]),
        "variable_name" => Some(vec![text(node).trim_start_matches('$').to_owned()]),
        "this" | "self" | "super" | "base" | "relative_scope" | "crate" => Some(vec![text(node)]),
        "member_expression" => join(
            node.child_by_field_name("object"),
            node.child_by_field_name("property"),
        ),
        "attribute" => join(
            node.child_by_field_name("object"),
            node.child_by_field_name("attribute"),
        ),
        "selector_expression" => join(
            node.child_by_field_name("operand"),
            node.child_by_field_name("field"),
        ),
        "member_access_expression" => join(
            node.child_by_field_name("expression"),
            node.child_by_field_name("name"),
        ),
        "field_access" => join(
            node.child_by_field_name("object"),
            node.child_by_field_name("field"),
        ),
        "field_expression" => join(
            node.child_by_field_name("value")
                .or_else(|| node.child_by_field_name("argument")),
            node.child_by_field_name("field"),
        ),
        "scope_resolution" => join(
            node.child_by_field_name("scope"),
            node.child_by_field_name("name"),
        ),
        "scoped_identifier" | "scoped_type_identifier" => join(
            node.child_by_field_name("path")
                .or_else(|| node.child_by_field_name("scope")),
            node.child_by_field_name("name"),
        ),
        "qualified_identifier" => join(
            node.child_by_field_name("scope"),
            node.child_by_field_name("name"),
        ),
        "qualified_name" => {
            let head = node
                .child_by_field_name("qualifier")
                .or_else(|| node.child_by_field_name("prefix"));
            match head {
                Some(head) => join(
                    Some(head),
                    node.child_by_field_name("name")
                        .or_else(|| node.named_child(node.named_child_count().saturating_sub(1))),
                ),
                None => Some(vec![text(node)]),
            }
        }
        "namespace_name" => {
            let mut cursor = node.walk();
            let parts: Vec<String> = node
                .named_children(&mut cursor)
                .map(|child| src[child.byte_range()].to_owned())
                .collect();
            (!parts.is_empty()).then_some(parts)
        }
        "dotted_name" => {
            let mut cursor = node.walk();
            let parts: Vec<String> = node
                .named_children(&mut cursor)
                .map(|child| src[child.byte_range()].to_owned())
                .collect();
            (!parts.is_empty()).then_some(parts)
        }
        "generic_type" | "generic_name" => node
            .child_by_field_name("name")
            .or_else(|| node.named_child(0))
            .and_then(|inner| path_segments(inner, src)),
        "parenthesized_expression" => node
            .named_child(0)
            .and_then(|inner| path_segments(inner, src)),
        _ => None,
    }
}

/// Classifies a receiver expression.
pub(super) fn receiver_from(node: Node<'_>, src: &str) -> Receiver {
    match path_segments(node, src) {
        Some(segments) if segments.len() == 1 => match segments[0].as_str() {
            "this" | "self" => Receiver::SelfValue,
            "super" | "base" | "parent" => Receiver::Super,
            _ => Receiver::Path(segments),
        },
        Some(segments) if matches!(segments[0].as_str(), "this" | "self") => Receiver::Expr,
        Some(segments) => Receiver::Path(segments),
        None => Receiver::Expr,
    }
}

/// Registers a dynamic call whose callee is the expression `callee`.
pub(super) fn dynamic_call(c: &mut Collector<'_>, callee: Node<'_>) {
    let text = c.text(callee);
    let name: String = text.chars().take(48).collect();
    c.call(CallSpec {
        name,
        kind: CallKind::Dynamic,
        receiver: Receiver::None,
        range: (callee.start_byte(), callee.end_byte()),
    });
}

/// Strips generic arguments, pointers, references and nullable markers from a type name.
pub(super) fn simple_type(text: &str) -> Option<String> {
    let base = text.split('<').next()?.trim();
    let base = base.trim_start_matches(['&', '*']).trim();
    let base = base.trim_end_matches(['?', '*', '&', '[', ']']).trim();
    let base = base.trim_start_matches("const ").trim();
    (!base.is_empty()
        && base
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '.' | ':' | '\\')))
    .then(|| base.to_owned())
}

pub(super) fn unquote(text: &str) -> &str {
    text.trim_matches(|c| matches!(c, '"' | '\'' | '`'))
}

pub(super) fn last_segment(path: &str) -> &str {
    path.rsplit(['.', ':', '\\', '/']).next().unwrap_or(path)
}
