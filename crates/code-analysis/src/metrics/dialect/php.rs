use tree_sitter::{Language, Node};

use super::{name_from_context, named_children, text, Control, Dialect};
use crate::metrics::model::FunctionKind;

pub struct Php;

const CLASSES: [&str; 4] = [
    "class_declaration",
    "interface_declaration",
    "trait_declaration",
    "enum_declaration",
];
const PRIMITIVES: [&str; 5] = ["int", "float", "string", "bool", "array"];

fn parameters<'t>(function: Node<'t>) -> Vec<Node<'t>> {
    function
        .child_by_field_name("parameters")
        .map(named_children)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| {
            matches!(
                p.kind(),
                "simple_parameter" | "variadic_parameter" | "property_promotion_parameter"
            )
        })
        .collect()
}

impl Dialect for Php {
    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_php::LANGUAGE_PHP.into()
    }

    fn function_kind(&self, node: Node<'_>) -> Option<FunctionKind> {
        if !node.is_named() {
            return None;
        }
        match node.kind() {
            "function_definition" => Some(FunctionKind::Function),
            "method_declaration" => Some(FunctionKind::Method),
            "anonymous_function" | "arrow_function" => Some(FunctionKind::Closure),
            _ => None,
        }
    }

    fn is_class(&self, node: Node<'_>) -> bool {
        node.is_named() && CLASSES.contains(&node.kind())
    }

    fn class_name(&self, node: Node<'_>, source: &[u8]) -> String {
        node.child_by_field_name("name")
            .map(|n| text(n, source).to_owned())
            .unwrap_or_else(|| "<class>".into())
    }

    fn function_name(&self, node: Node<'_>, source: &[u8]) -> String {
        match node.child_by_field_name("name") {
            Some(name) => text(name, source).to_owned(),
            None => name_from_context(node, source, "<closure>"),
        }
    }

    fn control(&self, node: Node<'_>, _source: &[u8]) -> Control {
        match node.kind() {
            "if_statement" => Control::If,
            "else_if_clause" => Control::ElseIf,
            "else_clause" => Control::Else,
            "for_statement" | "foreach_statement" | "while_statement" | "do_statement" => {
                Control::Loop
            }
            "switch_statement" | "match_expression" => Control::Switch,
            "case_statement" | "match_conditional_expression" => Control::Case,
            "catch_clause" => Control::Catch,
            "conditional_expression" => Control::Ternary,
            _ => Control::None,
        }
    }

    fn boolean_kinds(&self) -> &'static [&'static str] {
        &["binary_expression"]
    }

    fn is_comment(&self, node: Node<'_>) -> bool {
        node.kind() == "comment"
    }

    fn is_block(&self, node: Node<'_>) -> bool {
        matches!(
            node.kind(),
            "compound_statement" | "colon_block" | "switch_block" | "match_block"
        )
    }

    fn params(&self, function: Node<'_>, _source: &[u8]) -> u32 {
        parameters(function).len() as u32
    }

    fn receiver(&self, method: Node<'_>, _source: &[u8]) -> Option<String> {
        let is_static = named_children(method)
            .iter()
            .any(|c| c.kind() == "static_modifier");
        (method.kind() == "method_declaration" && !is_static).then(|| "$this".into())
    }

    fn member(&self, node: Node<'_>, source: &[u8], receiver: &str) -> Option<String> {
        if node.kind() != "member_access_expression" {
            return None;
        }
        let object = node.child_by_field_name("object")?;
        if text(object, source) != receiver {
            return None;
        }
        let name = node.child_by_field_name("name")?;
        (name.kind() == "name").then(|| text(name, source).to_owned())
    }

    fn is_negation(&self, node: Node<'_>) -> bool {
        node.kind() == "unary_op_expression"
            && node
                .child_by_field_name("operator")
                .is_some_and(|o| o.kind() == "!")
    }

    fn typed_params(&self, function: Node<'_>, source: &[u8]) -> (u32, u32) {
        let mut typed = 0;
        let mut primitive = 0;
        for param in parameters(function) {
            let Some(ty) = param.child_by_field_name("type") else {
                continue;
            };
            typed += 1;
            let bare = text(ty, source).trim_start_matches('?').trim();
            primitive += u32::from(PRIMITIVES.contains(&bare));
        }
        (typed, primitive)
    }

    fn is_constructor(&self, _function: Node<'_>, name: &str) -> bool {
        name == "__construct"
    }

    fn inherited_signature(&self, _function: Node<'_>, _source: &[u8]) -> bool {
        false
    }
}
