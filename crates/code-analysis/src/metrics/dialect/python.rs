use tree_sitter::{Language, Node};

use super::{name_from_context, named_children, text, Control, Dialect};
use crate::metrics::model::FunctionKind;

pub struct Python;

fn class_body_member(node: Node<'_>) -> bool {
    // `def` directly in a class body, possibly under decorators.
    let mut parent = node.parent();
    if parent.is_some_and(|p| p.kind() == "decorated_definition") {
        parent = parent.and_then(|p| p.parent());
    }
    parent
        .filter(|p| p.kind() == "block")
        .and_then(|p| p.parent())
        .is_some_and(|g| g.kind() == "class_definition")
}

impl Dialect for Python {
    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_python::LANGUAGE.into()
    }

    fn function_kind(&self, node: Node<'_>) -> Option<FunctionKind> {
        // Keywords such as `function`, `class` and `lambda` share their node's kind name.
        if !node.is_named() {
            return None;
        }
        match node.kind() {
            "function_definition" => Some(if class_body_member(node) {
                FunctionKind::Method
            } else {
                FunctionKind::Function
            }),
            "lambda" => Some(FunctionKind::Closure),
            _ => None,
        }
    }

    fn is_class(&self, node: Node<'_>) -> bool {
        node.is_named() && node.kind() == "class_definition"
    }

    fn class_name(&self, node: Node<'_>, source: &[u8]) -> String {
        node.child_by_field_name("name")
            .map(|n| text(n, source).to_owned())
            .unwrap_or_else(|| "<class>".into())
    }

    fn function_name(&self, node: Node<'_>, source: &[u8]) -> String {
        match node.child_by_field_name("name") {
            Some(name) => text(name, source).to_owned(),
            None => name_from_context(node, source, "<lambda>"),
        }
    }

    fn control(&self, node: Node<'_>, source: &[u8]) -> Control {
        match node.kind() {
            "if_statement" => Control::If,
            "elif_clause" => Control::ElseIf,
            "else_clause" => Control::Else,
            "for_statement" | "while_statement" => Control::Loop,
            "match_statement" => Control::Switch,
            "case_clause" => {
                let wildcard = named_children(node).first().is_some_and(|pattern| {
                    pattern.kind() == "case_pattern" && text(*pattern, source).trim() == "_"
                });
                if wildcard {
                    Control::None
                } else {
                    Control::Case
                }
            }
            "except_clause" | "except_group_clause" => Control::Catch,
            "conditional_expression" => Control::Ternary,
            "if_clause" => Control::Flat,
            _ => Control::None,
        }
    }

    fn boolean_kinds(&self) -> &'static [&'static str] {
        &["boolean_operator"]
    }

    fn is_comment(&self, node: Node<'_>) -> bool {
        node.kind() == "comment"
    }

    fn is_docstring(&self, node: Node<'_>, _source: &[u8]) -> bool {
        if node.kind() != "expression_statement" {
            return false;
        }
        let only_string = node.named_child_count() == 1
            && node
                .named_child(0)
                .is_some_and(|c| matches!(c.kind(), "string" | "concatenated_string"));
        if !only_string {
            return false;
        }
        let Some(parent) = node.parent() else {
            return false;
        };
        let first = named_children(parent)
            .first()
            .is_some_and(|c| c.id() == node.id());
        let owner_ok = match parent.kind() {
            "module" => true,
            "block" => parent
                .parent()
                .is_some_and(|g| matches!(g.kind(), "function_definition" | "class_definition")),
            _ => false,
        };
        first && owner_ok
    }

    fn is_block(&self, node: Node<'_>) -> bool {
        node.kind() == "block"
    }

    fn params(&self, function: Node<'_>, source: &[u8]) -> u32 {
        let Some(list) = function.child_by_field_name("parameters") else {
            return 0;
        };
        let params: Vec<Node<'_>> = named_children(list)
            .into_iter()
            .filter(|p| !matches!(p.kind(), "keyword_separator" | "positional_separator"))
            .collect();
        let receiver = self.function_kind(function) == Some(FunctionKind::Method)
            && !is_static(function, source)
            && !params.is_empty();
        (params.len() - usize::from(receiver)) as u32
    }

    fn is_negation(&self, node: Node<'_>) -> bool {
        node.kind() == "not_operator"
    }

    fn typed_params(&self, function: Node<'_>, source: &[u8]) -> (u32, u32) {
        let Some(list) = function.child_by_field_name("parameters") else {
            return (0, 0);
        };
        let mut typed = 0;
        let mut primitive = 0;
        for param in named_children(list) {
            if !matches!(param.kind(), "typed_parameter" | "typed_default_parameter") {
                continue;
            }
            let Some(ty) = param.child_by_field_name("type") else {
                continue;
            };
            typed += 1;
            primitive += u32::from(matches!(
                text(ty, source).trim(),
                "int" | "float" | "str" | "bool" | "bytes" | "complex"
            ));
        }
        (typed, primitive)
    }

    fn is_constructor(&self, _function: Node<'_>, name: &str) -> bool {
        name == "__init__"
    }

    fn inherited_signature(&self, function: Node<'_>, source: &[u8]) -> bool {
        function
            .parent()
            .filter(|p| p.kind() == "decorated_definition")
            .is_some_and(|p| {
                named_children(p).iter().any(|d| {
                    d.kind() == "decorator"
                        && matches!(
                            text(*d, source).trim(),
                            "@override" | "@typing.override" | "@typing_extensions.override"
                        )
                })
            })
    }

    fn receiver(&self, method: Node<'_>, source: &[u8]) -> Option<String> {
        if is_static(method, source) {
            return None;
        }
        let list = method.child_by_field_name("parameters")?;
        let first = named_children(list).into_iter().next()?;
        let name = match first.kind() {
            "identifier" => first,
            _ => first.child_by_field_name("name").unwrap_or(first),
        };
        Some(text(name, source).to_owned())
    }

    fn member(&self, node: Node<'_>, source: &[u8], receiver: &str) -> Option<String> {
        if node.kind() != "attribute" {
            return None;
        }
        let object = node.child_by_field_name("object")?;
        if object.kind() != "identifier" || text(object, source) != receiver {
            return None;
        }
        Some(text(node.child_by_field_name("attribute")?, source).to_owned())
    }
}

/// A `@staticmethod` has no receiver.
fn is_static(function: Node<'_>, source: &[u8]) -> bool {
    function
        .parent()
        .filter(|p| p.kind() == "decorated_definition")
        .is_some_and(|p| {
            named_children(p)
                .iter()
                .any(|d| d.kind() == "decorator" && text(*d, source).trim() == "@staticmethod")
        })
}
