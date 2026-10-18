use tree_sitter::{Language, Node};

use super::members::{children_of, implicit_member, Grammar};
use super::{named_children, text, Control, Dialect};
use crate::metrics::model::FunctionKind;

pub struct Kotlin;

const CLASS_BODIES: [&str; 2] = ["class_body", "enum_class_body"];
const PRIMITIVES: [&str; 13] = [
    "Int", "Long", "Short", "Byte", "Double", "Float", "Boolean", "Char", "String", "UInt",
    "ULong", "UShort", "UByte",
];

fn child<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    children_of(node, kind).into_iter().next()
}

/// The token before `node` among its parent's children is `else`.
fn after_else(node: Node<'_>) -> bool {
    node.prev_sibling().is_some_and(|p| p.kind() == "else")
}

/// A declaration directly in the body of a class, object or companion object.
fn in_class_body(node: Node<'_>) -> bool {
    node.parent()
        .is_some_and(|p| CLASS_BODIES.contains(&p.kind()))
}

fn variable_names(declaration: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    match declaration.kind() {
        "variable_declaration" => {
            if let Some(name) = child(declaration, "identifier") {
                out.push(text(name, source).to_owned());
            }
        }
        "multi_variable_declaration" => {
            for part in named_children(declaration) {
                variable_names(part, source, out);
            }
        }
        _ => {}
    }
}

fn parameter_names(list: Option<Node<'_>>, source: &[u8], out: &mut Vec<String>) {
    let Some(list) = list else {
        return;
    };
    for param in named_children(list) {
        match param.kind() {
            "parameter" | "class_parameter" => {
                if let Some(name) = child(param, "identifier") {
                    out.push(text(name, source).to_owned());
                }
            }
            _ => variable_names(param, source, out),
        }
    }
}

fn is_reference(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let first = parent.named_child(0).is_some_and(|c| c.id() == node.id());
    match parent.kind() {
        // `a.b`: only `a` is a name in scope.
        "navigation_expression" | "call_expression" => first,
        "value_argument" => node.next_sibling().is_none_or(|s| s.kind() != "="),
        "variable_declaration"
        | "parameter"
        | "class_parameter"
        | "function_declaration"
        | "class_declaration"
        | "object_declaration"
        | "user_type"
        | "qualified_identifier"
        | "import"
        | "package_header"
        | "type_alias"
        | "catch_block"
        | "label"
        | "annotation"
        | "lambda_parameters"
        | "enum_entry"
        | "type_parameter" => false,
        _ => true,
    }
}

fn scope_names(scope: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    match scope.kind() {
        "function_declaration" | "secondary_constructor" | "anonymous_function" => {
            parameter_names(child(scope, "function_value_parameters"), source, out)
        }
        "lambda_literal" => match child(scope, "lambda_parameters") {
            Some(list) => parameter_names(Some(list), source, out),
            None => out.push("it".into()),
        },
        "block" | "statements" | "source_file" => {
            for statement in children_of(scope, "property_declaration") {
                for declaration in named_children(statement) {
                    variable_names(declaration, source, out);
                }
            }
        }
        "for_statement" => {
            for declaration in named_children(scope) {
                variable_names(declaration, source, out);
            }
        }
        "catch_block" => {
            if let Some(name) = child(scope, "identifier") {
                out.push(text(name, source).to_owned());
            }
        }
        _ => {}
    }
}

fn member_names(body: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    for member in named_children(body) {
        match member.kind() {
            "property_declaration" => {
                for declaration in named_children(member) {
                    variable_names(declaration, source, out);
                }
            }
            "function_declaration" => {
                if let Some(name) = member.child_by_field_name("name") {
                    out.push(text(name, source).to_owned());
                }
            }
            _ => {}
        }
    }
    // `class A(val x: Int)`: constructor parameters with `val` or `var` are properties.
    let constructor = body.parent().and_then(|c| child(c, "primary_constructor"));
    if let Some(list) = constructor.and_then(|c| child(c, "class_parameters")) {
        for param in children_of(list, "class_parameter") {
            let mut cursor = param.walk();
            let property = param
                .children(&mut cursor)
                .any(|t| matches!(t.kind(), "val" | "var"));
            if let Some(name) = child(param, "identifier").filter(|_| property) {
                out.push(text(name, source).to_owned());
            }
        }
    }
}

const GRAMMAR: Grammar = Grammar {
    class_bodies: &CLASS_BODIES,
    is_reference,
    scope_names,
    member_names,
};

fn parameter_list(function: Node<'_>) -> Option<Node<'_>> {
    child(function, "function_value_parameters").or_else(|| child(function, "lambda_parameters"))
}

impl Dialect for Kotlin {
    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_kotlin_ng::LANGUAGE.into()
    }

    fn function_kind(&self, node: Node<'_>) -> Option<FunctionKind> {
        if !node.is_named() {
            return None;
        }
        match node.kind() {
            "function_declaration" => Some(if in_class_body(node) {
                FunctionKind::Method
            } else {
                FunctionKind::Function
            }),
            "secondary_constructor" => Some(FunctionKind::Method),
            "lambda_literal" | "anonymous_function" => Some(FunctionKind::Closure),
            _ => None,
        }
    }

    fn is_class(&self, node: Node<'_>) -> bool {
        node.is_named() && matches!(node.kind(), "class_declaration" | "object_declaration")
    }

    fn class_name(&self, node: Node<'_>, source: &[u8]) -> String {
        node.child_by_field_name("name")
            .map(|n| text(n, source).to_owned())
            .unwrap_or_else(|| "<class>".into())
    }

    fn function_name(&self, node: Node<'_>, source: &[u8]) -> String {
        if let Some(name) = node.child_by_field_name("name") {
            return text(name, source).to_owned();
        }
        if node.kind() == "secondary_constructor" {
            return "constructor".into();
        }
        let Some(parent) = node.parent() else {
            return "<lambda>".into();
        };
        match parent.kind() {
            "property_declaration" => {
                let mut names = Vec::new();
                for declaration in named_children(parent) {
                    variable_names(declaration, source, &mut names);
                }
                names
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| "<lambda>".into())
            }
            // `list.forEach { ... }`: named after the call it is passed to.
            "annotated_lambda" => parent
                .parent()
                .and_then(|call| call.named_child(0))
                .map(|callee| {
                    let callee = text(callee, source);
                    let short = callee.rsplit('.').next().unwrap_or(callee);
                    format!("{short} callback")
                })
                .unwrap_or_else(|| "<lambda>".into()),
            _ => "<lambda>".into(),
        }
    }

    fn body<'t>(&self, function: Node<'t>) -> Option<Node<'t>> {
        match function.kind() {
            "lambda_literal" => Some(function),
            "secondary_constructor" => child(function, "block"),
            _ => child(function, "function_body"),
        }
    }

    fn implicit_field(&self, statement: Node<'_>, child: Node<'_>) -> Option<&'static str> {
        match statement.kind() {
            "if_expression" if child.is_named() => Some(if after_else(child) {
                "alternative"
            } else {
                "consequence"
            }),
            "when_expression" => match child.kind() {
                "when_subject" => Some("subject"),
                "when_entry" => Some("body"),
                _ => None,
            },
            _ => None,
        }
    }

    fn control(&self, node: Node<'_>, _source: &[u8]) -> Control {
        let control = match node.kind() {
            "if_expression" => {
                let chained =
                    after_else(node) && node.parent().is_some_and(|p| p.kind() == "if_expression");
                return if chained {
                    Control::ElseIf
                } else {
                    Control::If
                };
            }
            "for_statement" | "while_statement" | "do_while_statement" => Control::Loop,
            "when_expression" => Control::Switch,
            "when_entry" => {
                let default = node.child(0).is_some_and(|c| c.kind() == "else");
                if default {
                    Control::None
                } else {
                    Control::Case
                }
            }
            "catch_block" => Control::Catch,
            _ => Control::None,
        };
        let else_branch = node.is_named()
            && after_else(node)
            && node.parent().is_some_and(|p| p.kind() == "if_expression");
        if control == Control::None && else_branch {
            Control::Else
        } else {
            control
        }
    }

    fn boolean_kinds(&self) -> &'static [&'static str] {
        &["binary_expression"]
    }

    fn is_comment(&self, node: Node<'_>) -> bool {
        matches!(node.kind(), "line_comment" | "block_comment")
    }

    fn is_block(&self, node: Node<'_>) -> bool {
        node.kind() == "block"
    }

    fn params(&self, function: Node<'_>, source: &[u8]) -> u32 {
        let mut names = Vec::new();
        parameter_names(parameter_list(function), source, &mut names);
        names.len() as u32
    }

    fn receiver(&self, method: Node<'_>, _source: &[u8]) -> Option<String> {
        in_class_body(method).then(|| "this".into())
    }

    fn member(&self, node: Node<'_>, source: &[u8], receiver: &str) -> Option<String> {
        if node.kind() == "navigation_expression" {
            let object = node.named_child(0)?;
            if object.kind() != "this_expression" || receiver != "this" {
                return None;
            }
            let name = node.named_child(1).filter(|n| n.kind() == "identifier")?;
            return Some(text(name, source).to_owned());
        }
        // `this.x` is counted above; its `x` is not a bare name.
        implicit_member(node, source, &GRAMMAR)
    }

    fn is_negation(&self, node: Node<'_>) -> bool {
        node.kind() == "unary_expression"
            && node
                .child_by_field_name("operator")
                .is_some_and(|o| o.kind() == "!")
    }

    fn typed_params(&self, function: Node<'_>, source: &[u8]) -> (u32, u32) {
        let Some(list) = parameter_list(function) else {
            return (0, 0);
        };
        let mut typed = 0;
        let mut primitive = 0;
        for param in children_of(list, "parameter") {
            let ty = child(param, "user_type").or_else(|| child(param, "nullable_type"));
            let Some(ty) = ty.or_else(|| child(param, "function_type")) else {
                continue;
            };
            typed += 1;
            let name = text(ty, source).trim().trim_end_matches('?');
            primitive += u32::from(PRIMITIVES.contains(&name));
        }
        (typed, primitive)
    }

    fn is_constructor(&self, function: Node<'_>, _name: &str) -> bool {
        function.kind() == "secondary_constructor"
    }

    fn inherited_signature(&self, function: Node<'_>, source: &[u8]) -> bool {
        child(function, "modifiers")
            .is_some_and(|m| text(m, source).split_whitespace().any(|w| w == "override"))
    }
}
