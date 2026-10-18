use tree_sitter::{Language, Node};

use super::{name_from_context, named_children, text, Control, Dialect};
use crate::metrics::model::FunctionKind;

pub struct Rust;

impl Dialect for Rust {
    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn function_kind(&self, node: Node<'_>) -> Option<FunctionKind> {
        // Keywords such as `function`, `class` and `lambda` share their node's kind name.
        if !node.is_named() {
            return None;
        }
        match node.kind() {
            "function_item" => {
                let in_impl = node
                    .parent()
                    .filter(|p| p.kind() == "declaration_list")
                    .and_then(|p| p.parent())
                    .is_some_and(|g| matches!(g.kind(), "impl_item" | "trait_item"));
                Some(if in_impl {
                    FunctionKind::Method
                } else {
                    FunctionKind::Function
                })
            }
            "closure_expression" => Some(FunctionKind::Closure),
            _ => None,
        }
    }

    fn is_class(&self, node: Node<'_>) -> bool {
        node.is_named() && matches!(node.kind(), "impl_item" | "trait_item")
    }

    fn class_name(&self, node: Node<'_>, source: &[u8]) -> String {
        let named = if node.kind() == "trait_item" {
            node.child_by_field_name("name")
        } else {
            node.child_by_field_name("type")
        };
        let name = named.map(|n| text(n, source)).unwrap_or("<impl>");
        name.split('<').next().unwrap_or(name).trim().to_owned()
    }

    fn function_name(&self, node: Node<'_>, source: &[u8]) -> String {
        match node.child_by_field_name("name") {
            Some(name) => text(name, source).to_owned(),
            None => name_from_context(node, source, "<closure>"),
        }
    }

    fn control(&self, node: Node<'_>, source: &[u8]) -> Control {
        match node.kind() {
            "if_expression" => {
                if node.parent().is_some_and(|p| p.kind() == "else_clause") {
                    Control::ElseIf
                } else {
                    Control::If
                }
            }
            "else_clause" => {
                let chained = named_children(node)
                    .iter()
                    .any(|c| c.kind() == "if_expression");
                if chained {
                    Control::None
                } else {
                    Control::Else
                }
            }
            "for_expression" | "while_expression" | "loop_expression" => Control::Loop,
            "match_expression" => Control::Switch,
            "match_arm" => {
                let wildcard = node
                    .child_by_field_name("pattern")
                    .is_some_and(|p| text(p, source).trim() == "_");
                if wildcard {
                    Control::None
                } else {
                    Control::Case
                }
            }
            _ => Control::None,
        }
    }

    fn boolean_kinds(&self) -> &'static [&'static str] {
        &["binary_expression", "let_chain"]
    }

    fn is_comment(&self, node: Node<'_>) -> bool {
        matches!(node.kind(), "line_comment" | "block_comment")
    }

    fn is_block(&self, node: Node<'_>) -> bool {
        node.kind() == "block"
    }

    fn params(&self, function: Node<'_>, _source: &[u8]) -> u32 {
        let Some(list) = function.child_by_field_name("parameters") else {
            return 0;
        };
        named_children(list)
            .iter()
            .filter(|p| !matches!(p.kind(), "self_parameter" | "attribute_item"))
            .count() as u32
    }

    fn is_negation(&self, node: Node<'_>) -> bool {
        node.kind() == "unary_expression" && node.child(0).is_some_and(|c| c.kind() == "!")
    }

    fn typed_params(&self, function: Node<'_>, source: &[u8]) -> (u32, u32) {
        let Some(list) = function.child_by_field_name("parameters") else {
            return (0, 0);
        };
        let mut typed = 0;
        let mut primitive = 0;
        for param in named_children(list) {
            let Some(ty) = param.child_by_field_name("type") else {
                continue;
            };
            typed += 1;
            let bare = text(ty, source)
                .trim_start_matches('&')
                .trim_start_matches("'_ ")
                .trim_start_matches("mut ")
                .trim();
            if ty.kind() == "primitive_type"
                || matches!(bare, "str" | "String" | "bool" | "char")
                || ty
                    .child_by_field_name("type")
                    .is_some_and(|inner| inner.kind() == "primitive_type")
            {
                primitive += 1;
            }
        }
        (typed, primitive)
    }

    fn is_constructor(&self, _function: Node<'_>, name: &str) -> bool {
        name == "new"
    }

    fn inherited_signature(&self, function: Node<'_>, _source: &[u8]) -> bool {
        // `impl Trait for Type`: the trait decides the signature.
        function
            .parent()
            .and_then(|list| list.parent())
            .is_some_and(|block| {
                block.kind() == "impl_item" && block.child_by_field_name("trait").is_some()
            })
    }

    fn receiver(&self, _method: Node<'_>, _source: &[u8]) -> Option<String> {
        Some("self".into())
    }

    fn member(&self, node: Node<'_>, source: &[u8], receiver: &str) -> Option<String> {
        if node.kind() != "field_expression" {
            return None;
        }
        let value = node.child_by_field_name("value")?;
        if text(value, source) != receiver {
            return None;
        }
        Some(text(node.child_by_field_name("field")?, source).to_owned())
    }
}
