use tree_sitter::{Language, Node};

use super::{name_from_context, named_children, text, Control, Dialect};
use crate::metrics::model::FunctionKind;

pub struct Go;

const PRIMITIVES: [&str; 18] = [
    "int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16", "uint32", "uint64",
    "float32", "float64", "bool", "string", "byte", "rune", "uintptr", "error",
];

/// The alternative of an enclosing `if`.
fn is_alternative(node: Node<'_>) -> bool {
    node.parent()
        .filter(|p| p.kind() == "if_statement")
        .and_then(|p| p.child_by_field_name("alternative"))
        .is_some_and(|a| a.id() == node.id())
}

/// The parameter declarations of a function's parameter list.
fn declarations<'t>(function: Node<'t>) -> Vec<Node<'t>> {
    function
        .child_by_field_name("parameters")
        .map(|list| {
            named_children(list)
                .into_iter()
                .filter(|p| {
                    matches!(
                        p.kind(),
                        "parameter_declaration" | "variadic_parameter_declaration"
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `a, b int` declares two parameters; an unnamed `int` declares one.
fn declared_count(declaration: Node<'_>) -> u32 {
    let mut cursor = declaration.walk();
    let names = declaration
        .children_by_field_name("name", &mut cursor)
        .count() as u32;
    names.max(1)
}

impl Dialect for Go {
    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_go::LANGUAGE.into()
    }

    fn function_kind(&self, node: Node<'_>) -> Option<FunctionKind> {
        if !node.is_named() {
            return None;
        }
        match node.kind() {
            "function_declaration" => Some(FunctionKind::Function),
            "method_declaration" => Some(FunctionKind::Method),
            "func_literal" => Some(FunctionKind::Closure),
            _ => None,
        }
    }

    /// Methods are declared outside the type, so there is no node that owns them.
    fn is_class(&self, _node: Node<'_>) -> bool {
        false
    }

    fn class_name(&self, _node: Node<'_>, _source: &[u8]) -> String {
        "<type>".into()
    }

    fn function_name(&self, node: Node<'_>, source: &[u8]) -> String {
        match node.child_by_field_name("name") {
            Some(name) => text(name, source).to_owned(),
            None => name_from_context(node, source, "<func>"),
        }
    }

    fn owner_hint(&self, method: Node<'_>, source: &[u8]) -> Option<String> {
        let list = method.child_by_field_name("receiver")?;
        let ty = named_children(list)
            .into_iter()
            .next()?
            .child_by_field_name("type")?;
        let written = text(ty, source).trim_start_matches('*');
        Some(
            written
                .split('[')
                .next()
                .unwrap_or(written)
                .trim()
                .to_owned(),
        )
    }

    fn synthesizes_classes(&self) -> bool {
        true
    }

    fn control(&self, node: Node<'_>, _source: &[u8]) -> Control {
        match node.kind() {
            "if_statement" if is_alternative(node) => Control::ElseIf,
            "if_statement" => Control::If,
            _ if is_alternative(node) => Control::Else,
            "for_statement" => Control::Loop,
            "expression_switch_statement" | "type_switch_statement" | "select_statement" => {
                Control::Switch
            }
            "expression_case" | "type_case" | "communication_case" => Control::Case,
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
        node.kind() == "block"
    }

    fn params(&self, function: Node<'_>, _source: &[u8]) -> u32 {
        declarations(function).into_iter().map(declared_count).sum()
    }

    fn receiver(&self, method: Node<'_>, source: &[u8]) -> Option<String> {
        let list = method.child_by_field_name("receiver")?;
        let declaration = named_children(list).into_iter().next()?;
        let name = declaration.child_by_field_name("name")?;
        Some(text(name, source).to_owned())
    }

    fn member(&self, node: Node<'_>, source: &[u8], receiver: &str) -> Option<String> {
        if node.kind() != "selector_expression" {
            return None;
        }
        let operand = node.child_by_field_name("operand")?;
        if text(operand, source) != receiver {
            return None;
        }
        Some(text(node.child_by_field_name("field")?, source).to_owned())
    }

    fn is_negation(&self, node: Node<'_>) -> bool {
        node.kind() == "unary_expression"
            && node
                .child_by_field_name("operator")
                .is_some_and(|o| o.kind() == "!")
    }

    fn typed_params(&self, function: Node<'_>, source: &[u8]) -> (u32, u32) {
        let mut typed = 0;
        let mut primitive = 0;
        for declaration in declarations(function) {
            let count = declared_count(declaration);
            typed += count;
            if let Some(ty) = declaration.child_by_field_name("type") {
                if PRIMITIVES.contains(&text(ty, source).trim()) {
                    primitive += count;
                }
            }
        }
        (typed, primitive)
    }

    fn is_constructor(&self, function: Node<'_>, name: &str) -> bool {
        function.kind() == "function_declaration"
            && name
                .strip_prefix("New")
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(|c: char| c.is_uppercase()))
    }

    fn inherited_signature(&self, _function: Node<'_>, _source: &[u8]) -> bool {
        false
    }
}
