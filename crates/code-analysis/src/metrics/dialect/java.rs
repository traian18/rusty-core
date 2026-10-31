use tree_sitter::{Language, Node};

use super::members::{children_of, implicit_member, Grammar};
use super::{name_from_context, named_children, text, Control, Dialect};
use crate::metrics::model::FunctionKind;

pub struct Java;

const CLASS_BODIES: [&str; 4] = [
    "class_body",
    "interface_body",
    "enum_body_declarations",
    "annotation_type_body",
];
const CLASSES: [&str; 4] = [
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
];
const PRIMITIVES: [&str; 9] = [
    "int", "long", "short", "byte", "double", "float", "boolean", "char", "String",
];

fn modifiers_contain(node: Node<'_>, source: &[u8], word: &str) -> bool {
    children_of(node, "modifiers")
        .first()
        .is_some_and(|m| text(*m, source).split_whitespace().any(|w| w == word))
}

/// The declarator names of a field or local variable declaration.
fn declarator_names(declaration: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    let mut cursor = declaration.walk();
    for declarator in declaration.children_by_field_name("declarator", &mut cursor) {
        if let Some(name) = declarator.child_by_field_name("name") {
            out.push(text(name, source).to_owned());
        }
    }
}

fn parameter_names(list: Option<Node<'_>>, source: &[u8], out: &mut Vec<String>) {
    let Some(list) = list else {
        return;
    };
    if list.kind() == "identifier" {
        out.push(text(list, source).to_owned());
        return;
    }
    for param in named_children(list) {
        match param.kind() {
            "identifier" => out.push(text(param, source).to_owned()),
            "formal_parameter" | "spread_parameter" => {
                let name = param.child_by_field_name("name").or_else(|| {
                    children_of(param, "variable_declarator")
                        .first()
                        .and_then(|d| d.child_by_field_name("name"))
                });
                if let Some(name) = name {
                    out.push(text(name, source).to_owned());
                }
            }
            _ => {}
        }
    }
}

fn is_reference(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let is_field = |name: &str| {
        parent
            .child_by_field_name(name)
            .is_some_and(|c| c.id() == node.id())
    };
    match parent.kind() {
        // `a.b`: only `a` is a name in scope.
        "field_access" => is_field("object"),
        // `helper()` names a sibling method; `x.helper()` does not.
        "method_invocation" => {
            is_field("object")
                || (is_field("name") && parent.child_by_field_name("object").is_none())
        }
        "variable_declarator" => !is_field("name"),
        "formal_parameter"
        | "spread_parameter"
        | "catch_formal_parameter"
        | "method_declaration"
        | "constructor_declaration"
        | "class_declaration"
        | "interface_declaration"
        | "enum_declaration"
        | "record_declaration"
        | "scoped_identifier"
        | "import_declaration"
        | "package_declaration"
        | "inferred_parameters"
        | "lambda_expression"
        | "labeled_statement"
        | "enum_constant"
        | "marker_annotation"
        | "annotation"
        | "method_reference" => false,
        "enhanced_for_statement" | "resource" => !is_field("name"),
        _ => true,
    }
}

fn scope_names(scope: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    match scope.kind() {
        "method_declaration" | "constructor_declaration" | "lambda_expression" => {
            parameter_names(scope.child_by_field_name("parameters"), source, out)
        }
        "block" | "constructor_body" | "switch_block_statement_group" => {
            for local in children_of(scope, "local_variable_declaration") {
                declarator_names(local, source, out);
            }
        }
        "for_statement" => {
            if let Some(init) = scope.child_by_field_name("init") {
                declarator_names(init, source, out);
            }
        }
        "enhanced_for_statement" | "catch_formal_parameter" | "resource" => {
            if let Some(name) = scope.child_by_field_name("name") {
                out.push(text(name, source).to_owned());
            }
        }
        "catch_clause" => {
            for param in children_of(scope, "catch_formal_parameter") {
                scope_names(param, source, out);
            }
        }
        "try_with_resources_statement" => {
            if let Some(resources) = scope.child_by_field_name("resources") {
                for resource in children_of(resources, "resource") {
                    scope_names(resource, source, out);
                }
            }
        }
        _ => {}
    }
}

fn member_names(body: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    for member in named_children(body) {
        match member.kind() {
            "field_declaration" | "constant_declaration" => declarator_names(member, source, out),
            "method_declaration" => {
                if let Some(name) = member.child_by_field_name("name") {
                    out.push(text(name, source).to_owned());
                }
            }
            _ => {}
        }
    }
    // `record Point(int x, int y)`: the components are fields.
    let record = body.parent().filter(|p| p.kind() == "record_declaration");
    parameter_names(
        record.and_then(|r| r.child_by_field_name("parameters")),
        source,
        out,
    );
}

const GRAMMAR: Grammar = Grammar {
    class_bodies: &CLASS_BODIES,
    is_reference,
    scope_names,
    member_names,
};

/// The alternative of an enclosing `if`.
fn is_alternative(node: Node<'_>) -> bool {
    node.parent()
        .filter(|p| p.kind() == "if_statement")
        .and_then(|p| p.child_by_field_name("alternative"))
        .is_some_and(|a| a.id() == node.id())
}

impl Dialect for Java {
    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_java::LANGUAGE.into()
    }

    fn function_kind(&self, node: Node<'_>) -> Option<FunctionKind> {
        if !node.is_named() {
            return None;
        }
        match node.kind() {
            "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration" => Some(FunctionKind::Method),
            "lambda_expression" => Some(FunctionKind::Closure),
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
            None => name_from_context(node, source, "<lambda>"),
        }
    }

    fn control(&self, node: Node<'_>, source: &[u8]) -> Control {
        match node.kind() {
            "if_statement" if is_alternative(node) => Control::ElseIf,
            "if_statement" => Control::If,
            _ if is_alternative(node) => Control::Else,
            "for_statement" | "enhanced_for_statement" | "while_statement" | "do_statement" => {
                Control::Loop
            }
            "switch_expression" => Control::Switch,
            "switch_label" => {
                if text(node, source).trim_start().starts_with("default") {
                    Control::None
                } else {
                    Control::Case
                }
            }
            "catch_clause" => Control::Catch,
            "ternary_expression" => Control::Ternary,
            _ => Control::None,
        }
    }

    fn boolean_kinds(&self) -> &'static [&'static str] {
        &["binary_expression"]
    }

    fn is_comment(&self, node: Node<'_>) -> bool {
        matches!(node.kind(), "line_comment" | "block_comment")
    }

    fn is_block(&self, node: Node<'_>) -> bool {
        matches!(node.kind(), "block" | "constructor_body" | "switch_block")
    }

    fn params(&self, function: Node<'_>, source: &[u8]) -> u32 {
        let mut names = Vec::new();
        parameter_names(
            function.child_by_field_name("parameters"),
            source,
            &mut names,
        );
        names.len() as u32
    }

    fn receiver(&self, method: Node<'_>, source: &[u8]) -> Option<String> {
        (method.kind() != "lambda_expression" && !modifiers_contain(method, source, "static"))
            .then(|| "this".into())
    }

    fn member(&self, node: Node<'_>, source: &[u8], receiver: &str) -> Option<String> {
        if node.kind() == "field_access" {
            let object = node.child_by_field_name("object")?;
            if object.kind() != "this" || receiver != "this" {
                return None;
            }
            return Some(text(node.child_by_field_name("field")?, source).to_owned());
        }
        if node.kind() == "method_invocation" {
            let object = node.child_by_field_name("object")?;
            if object.kind() != "this" || receiver != "this" {
                return None;
            }
            return Some(text(node.child_by_field_name("name")?, source).to_owned());
        }
        implicit_member(node, source, &GRAMMAR)
    }

    fn is_negation(&self, node: Node<'_>) -> bool {
        node.kind() == "unary_expression"
            && node
                .child_by_field_name("operator")
                .is_some_and(|o| o.kind() == "!")
    }

    fn typed_params(&self, function: Node<'_>, source: &[u8]) -> (u32, u32) {
        let Some(list) = function.child_by_field_name("parameters") else {
            return (0, 0);
        };
        let mut typed = 0;
        let mut primitive = 0;
        for param in named_children(list) {
            if !matches!(param.kind(), "formal_parameter" | "spread_parameter") {
                continue;
            }
            let Some(ty) = param
                .child_by_field_name("type")
                .or_else(|| param.named_child(0))
            else {
                continue;
            };
            typed += 1;
            primitive += u32::from(PRIMITIVES.contains(&text(ty, source).trim()));
        }
        (typed, primitive)
    }

    fn is_constructor(&self, function: Node<'_>, _name: &str) -> bool {
        matches!(
            function.kind(),
            "constructor_declaration" | "compact_constructor_declaration"
        )
    }

    fn inherited_signature(&self, function: Node<'_>, source: &[u8]) -> bool {
        children_of(function, "modifiers").first().is_some_and(|m| {
            named_children(*m)
                .iter()
                .any(|a| a.kind() == "marker_annotation" && text(*a, source).trim() == "@Override")
        })
    }
}
