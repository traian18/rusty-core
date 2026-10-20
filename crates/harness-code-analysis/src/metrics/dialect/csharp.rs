use tree_sitter::{Language, Node};

use super::members::{children_of, implicit_member, Grammar};
use super::{name_from_context, named_children, text, Control, Dialect};
use crate::metrics::model::FunctionKind;

pub struct CSharp;

const CLASSES: [&str; 5] = [
    "class_declaration",
    "interface_declaration",
    "struct_declaration",
    "record_declaration",
    "record_struct_declaration",
];
const PRIMITIVES: [&str; 14] = [
    "int", "uint", "long", "ulong", "short", "ushort", "byte", "sbyte", "double", "float",
    "decimal", "bool", "char", "string",
];

fn has_modifier(node: Node<'_>, source: &[u8], word: &str) -> bool {
    children_of(node, "modifier")
        .iter()
        .any(|m| text(*m, source).trim() == word)
}

/// The alternative of an enclosing `if`.
fn is_alternative(node: Node<'_>) -> bool {
    node.parent()
        .filter(|p| p.kind() == "if_statement")
        .and_then(|p| p.child_by_field_name("alternative"))
        .is_some_and(|a| a.id() == node.id())
}

fn parameters<'t>(function: Node<'t>) -> Vec<Node<'t>> {
    function
        .child_by_field_name("parameters")
        .map(|list| match list.kind() {
            "parameter_list" => children_of(list, "parameter"),
            // `x => ...`
            _ => vec![list],
        })
        .unwrap_or_default()
}

fn declarator_names(declaration: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    for declarator in children_of(declaration, "variable_declarator") {
        if let Some(name) = declarator.child_by_field_name("name") {
            out.push(text(name, source).to_owned());
        }
    }
}

fn parameter_names(function: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    let Some(list) = function.child_by_field_name("parameters") else {
        return;
    };
    if list.kind() != "parameter_list" {
        out.push(text(list, source).to_owned());
        return;
    }
    for param in children_of(list, "parameter") {
        if let Some(name) = param.child_by_field_name("name") {
            out.push(text(name, source).to_owned());
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
        "member_access_expression" => is_field("expression"),
        "variable_declarator" | "foreach_statement" | "declaration_expression" => {
            !is_field("name") && !is_field("left")
        }
        "parameter"
        | "catch_declaration"
        | "property_declaration"
        | "method_declaration"
        | "constructor_declaration"
        | "local_function_statement"
        | "class_declaration"
        | "interface_declaration"
        | "struct_declaration"
        | "record_declaration"
        | "qualified_name"
        | "alias_qualified_name"
        | "generic_name"
        | "name_colon"
        | "namespace_declaration"
        | "file_scoped_namespace_declaration"
        | "using_directive"
        | "attribute"
        | "implicit_parameter" => false,
        _ => true,
    }
}

fn scope_names(scope: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    match scope.kind() {
        "method_declaration"
        | "constructor_declaration"
        | "local_function_statement"
        | "lambda_expression"
        | "anonymous_method_expression" => parameter_names(scope, source, out),
        "block" | "switch_section" => {
            for local in children_of(scope, "local_declaration_statement") {
                for declaration in children_of(local, "variable_declaration") {
                    declarator_names(declaration, source, out);
                }
            }
        }
        "for_statement" | "using_statement" => {
            for declaration in children_of(scope, "variable_declaration") {
                declarator_names(declaration, source, out);
            }
        }
        "foreach_statement" => {
            if let Some(left) = scope.child_by_field_name("left") {
                out.push(text(left, source).to_owned());
            }
        }
        "catch_clause" => {
            for declaration in children_of(scope, "catch_declaration") {
                if let Some(name) = declaration.child_by_field_name("name") {
                    out.push(text(name, source).to_owned());
                }
            }
        }
        _ => {}
    }
}

fn member_names(body: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    for member in named_children(body) {
        match member.kind() {
            "field_declaration" => {
                for declaration in children_of(member, "variable_declaration") {
                    declarator_names(declaration, source, out);
                }
            }
            "property_declaration" | "method_declaration" => {
                if let Some(name) = member.child_by_field_name("name") {
                    out.push(text(name, source).to_owned());
                }
            }
            _ => {}
        }
    }
    // `record Point(int X, int Y)`: the components are properties.
    if let Some(record) = body.parent().filter(|p| p.kind() == "record_declaration") {
        if let Some(list) = children_of(record, "parameter_list").first() {
            for param in children_of(*list, "parameter") {
                if let Some(name) = param.child_by_field_name("name") {
                    out.push(text(name, source).to_owned());
                }
            }
        }
    }
}

const GRAMMAR: Grammar = Grammar {
    class_bodies: &["declaration_list"],
    is_reference,
    scope_names,
    member_names,
};

impl Dialect for CSharp {
    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_c_sharp::LANGUAGE.into()
    }

    fn function_kind(&self, node: Node<'_>) -> Option<FunctionKind> {
        if !node.is_named() {
            return None;
        }
        match node.kind() {
            "method_declaration"
            | "constructor_declaration"
            | "destructor_declaration"
            | "operator_declaration"
            | "conversion_operator_declaration" => Some(FunctionKind::Method),
            "local_function_statement" => Some(FunctionKind::Function),
            "lambda_expression" | "anonymous_method_expression" => Some(FunctionKind::Closure),
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
            "for_statement" | "foreach_statement" | "while_statement" | "do_statement" => {
                Control::Loop
            }
            "switch_statement" | "switch_expression" => Control::Switch,
            "switch_section" => {
                if text(node, source).trim_start().starts_with("default") {
                    Control::None
                } else {
                    Control::Case
                }
            }
            "switch_expression_arm" => {
                let pattern = text(node, source).split("=>").next().unwrap_or("").trim();
                if pattern == "_" {
                    Control::None
                } else {
                    Control::Case
                }
            }
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
        matches!(node.kind(), "block" | "switch_body")
    }

    fn params(&self, function: Node<'_>, _source: &[u8]) -> u32 {
        parameters(function).len() as u32
    }

    fn receiver(&self, method: Node<'_>, source: &[u8]) -> Option<String> {
        (method.kind() != "local_function_statement"
            && !has_modifier(method, source, "static")
            && !matches!(
                method.kind(),
                "lambda_expression" | "anonymous_method_expression"
            ))
        .then(|| "this".into())
    }

    fn member(&self, node: Node<'_>, source: &[u8], receiver: &str) -> Option<String> {
        if node.kind() != "member_access_expression" {
            return implicit_member(node, source, &GRAMMAR);
        }
        let object = node.child_by_field_name("expression")?;
        if text(object, source) != receiver {
            return None;
        }
        Some(text(node.child_by_field_name("name")?, source).to_owned())
    }

    fn is_negation(&self, node: Node<'_>) -> bool {
        node.kind() == "prefix_unary_expression" && node.child(0).is_some_and(|c| c.kind() == "!")
    }

    fn typed_params(&self, function: Node<'_>, source: &[u8]) -> (u32, u32) {
        let mut typed = 0;
        let mut primitive = 0;
        for param in parameters(function) {
            let Some(ty) = param.child_by_field_name("type") else {
                continue;
            };
            typed += 1;
            primitive += u32::from(PRIMITIVES.contains(&text(ty, source).trim()));
        }
        (typed, primitive)
    }

    fn is_constructor(&self, function: Node<'_>, _name: &str) -> bool {
        function.kind() == "constructor_declaration"
    }

    fn inherited_signature(&self, function: Node<'_>, source: &[u8]) -> bool {
        has_modifier(function, source, "override")
            || named_children(function)
                .iter()
                .any(|c| c.kind() == "explicit_interface_specifier")
    }
}
