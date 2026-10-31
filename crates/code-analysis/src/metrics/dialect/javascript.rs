use tree_sitter::{Language, Node};

use super::{name_from_context, named_children, text, Control, Dialect};
use crate::metrics::model::FunctionKind;

pub struct Js {
    pub typescript: bool,
}

impl Dialect for Js {
    fn grammar(&self, path: &str) -> Language {
        if !self.typescript {
            tree_sitter_javascript::LANGUAGE.into()
        } else if path.ends_with(".tsx") {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        }
    }

    fn function_kind(&self, node: Node<'_>) -> Option<FunctionKind> {
        // Keywords such as `function`, `class` and `lambda` share their node's kind name.
        if !node.is_named() {
            return None;
        }
        match node.kind() {
            "method_definition" => Some(FunctionKind::Method),
            "function_declaration" | "generator_function_declaration" => {
                Some(FunctionKind::Function)
            }
            "arrow_function" | "function_expression" | "function" | "generator_function" => {
                // A function stored in a class field is how many classes write their methods.
                let field = node.parent().is_some_and(|p| {
                    matches!(p.kind(), "public_field_definition" | "field_definition")
                });
                Some(if field {
                    FunctionKind::Method
                } else {
                    FunctionKind::Closure
                })
            }
            _ => None,
        }
    }

    fn is_class(&self, node: Node<'_>) -> bool {
        node.is_named()
            && matches!(
                node.kind(),
                "class_declaration" | "abstract_class_declaration" | "class"
            )
    }

    fn class_name(&self, node: Node<'_>, source: &[u8]) -> String {
        match node.child_by_field_name("name") {
            Some(name) => text(name, source).to_owned(),
            None => name_from_context(node, source, "<class>"),
        }
    }

    fn function_name(&self, node: Node<'_>, source: &[u8]) -> String {
        match node.child_by_field_name("name") {
            Some(name) => text(name, source).to_owned(),
            None => name_from_context(node, source, "<anonymous>"),
        }
    }

    fn control(&self, node: Node<'_>, _source: &[u8]) -> Control {
        match node.kind() {
            "if_statement" => {
                if node.parent().is_some_and(|p| p.kind() == "else_clause") {
                    Control::ElseIf
                } else {
                    Control::If
                }
            }
            "else_clause" => {
                let chained = named_children(node)
                    .iter()
                    .any(|c| c.kind() == "if_statement");
                if chained {
                    Control::None
                } else {
                    Control::Else
                }
            }
            "for_statement" | "for_in_statement" | "while_statement" | "do_statement" => {
                Control::Loop
            }
            "switch_statement" => Control::Switch,
            "switch_case" => Control::Case,
            "catch_clause" => Control::Catch,
            "ternary_expression" => Control::Ternary,
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
        node.kind() == "statement_block"
    }

    fn params(&self, function: Node<'_>, _source: &[u8]) -> u32 {
        if let Some(single) = function.child_by_field_name("parameter") {
            return u32::from(single.is_named());
        }
        let Some(list) = function.child_by_field_name("parameters") else {
            return 0;
        };
        named_children(list)
            .iter()
            .filter(|p| {
                // TypeScript's `this: T` declares the receiver's type, not an argument.
                let pattern = p.child_by_field_name("pattern");
                pattern.is_none_or(|pattern| pattern.kind() != "this") && p.kind() != "decorator"
            })
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
            if param
                .child_by_field_name("pattern")
                .is_some_and(|p| p.kind() == "this")
            {
                continue;
            }
            let Some(annotation) = param.child_by_field_name("type") else {
                continue;
            };
            typed += 1;
            let ty = annotation.named_child(0).unwrap_or(annotation);
            let scalar = match ty.kind() {
                "predefined_type" => matches!(
                    text(ty, source),
                    "string" | "number" | "boolean" | "bigint" | "symbol"
                ),
                "literal_type" => true,
                _ => false,
            };
            primitive += u32::from(scalar);
        }
        (typed, primitive)
    }

    fn is_constructor(&self, _function: Node<'_>, name: &str) -> bool {
        name == "constructor"
    }

    fn inherited_signature(&self, function: Node<'_>, _source: &[u8]) -> bool {
        let mut cursor = function.walk();
        let overrides = function
            .children(&mut cursor)
            .any(|c| c.kind() == "override_modifier");
        overrides
    }

    fn receiver(&self, _method: Node<'_>, _source: &[u8]) -> Option<String> {
        Some("this".into())
    }

    fn member(&self, node: Node<'_>, source: &[u8], receiver: &str) -> Option<String> {
        if node.kind() != "member_expression" {
            return None;
        }
        let object = node.child_by_field_name("object")?;
        if text(object, source) != receiver {
            return None;
        }
        Some(text(node.child_by_field_name("property")?, source).to_owned())
    }
}
