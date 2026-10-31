use tree_sitter::{Language, Node};

use super::members::{children_of, implicit_member, Grammar};
use super::{named_children, text, Control, Dialect};
use crate::metrics::model::FunctionKind;

pub struct Ruby;

fn parent_kind(node: Node<'_>) -> &'static str {
    node.parent().map_or("", |p| p.kind())
}

/// A `{ }` or `do end` block that is a lambda's body is measured with the lambda.
fn is_lambda_body(node: Node<'_>) -> bool {
    parent_kind(node) == "lambda"
}

fn parameter_list(function: Node<'_>) -> Vec<Node<'_>> {
    function
        .child_by_field_name("parameters")
        .map(named_children)
        .unwrap_or_default()
}

fn parameter_names(function: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    for param in parameter_list(function) {
        match param.kind() {
            "identifier" => out.push(text(param, source).to_owned()),
            "optional_parameter"
            | "keyword_parameter"
            | "splat_parameter"
            | "hash_splat_parameter"
            | "block_parameter" => {
                if let Some(name) = param.child_by_field_name("name") {
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
        // `x.helper` names no sibling method.
        "call" => !is_field("method") || parent.child_by_field_name("receiver").is_none(),
        "assignment" => !is_field("left"),
        "method"
        | "singleton_method"
        | "class"
        | "module"
        | "method_parameters"
        | "block_parameters"
        | "lambda_parameters"
        | "optional_parameter"
        | "keyword_parameter"
        | "splat_parameter"
        | "hash_splat_parameter"
        | "block_parameter"
        | "destructured_parameter"
        | "exception_variable"
        | "for"
        | "scope_resolution" => false,
        _ => true,
    }
}

fn scope_names(scope: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    match scope.kind() {
        "method" | "singleton_method" | "block" | "do_block" | "lambda" => {
            parameter_names(scope, source, out)
        }
        "body_statement" | "block_body" | "then" | "do" | "begin" => {
            for assignment in children_of(scope, "assignment") {
                if let Some(left) = assignment
                    .child_by_field_name("left")
                    .filter(|l| l.kind() == "identifier")
                {
                    out.push(text(left, source).to_owned());
                }
            }
        }
        "exception_variable" => {
            for name in children_of(scope, "identifier") {
                out.push(text(name, source).to_owned());
            }
        }
        _ => {}
    }
}

/// Sibling methods and the readers `attr_*` declares. The scope is the `class` or `module`
/// itself, because method bodies are `body_statement`s too.
fn member_names(class: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    let Some(body) = class.child_by_field_name("body") else {
        return;
    };
    for member in named_children(body) {
        match member.kind() {
            "method" => {
                if let Some(name) = member.child_by_field_name("name") {
                    out.push(text(name, source).to_owned());
                }
            }
            "call" => {
                let accessor = member
                    .child_by_field_name("method")
                    .map(|m| text(m, source))
                    .is_some_and(|m| matches!(m, "attr_reader" | "attr_writer" | "attr_accessor"));
                if let (true, Some(arguments)) = (accessor, member.child_by_field_name("arguments"))
                {
                    for symbol in children_of(arguments, "simple_symbol") {
                        out.push(text(symbol, source).trim_start_matches(':').to_owned());
                    }
                }
            }
            _ => {}
        }
    }
}

const GRAMMAR: Grammar = Grammar {
    class_bodies: &["class", "module"],
    is_reference,
    scope_names,
    member_names,
};

impl Dialect for Ruby {
    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_ruby::LANGUAGE.into()
    }

    fn function_kind(&self, node: Node<'_>) -> Option<FunctionKind> {
        if !node.is_named() {
            return None;
        }
        match node.kind() {
            "method" => Some(FunctionKind::Method),
            "singleton_method" => Some(FunctionKind::Function),
            "lambda" => Some(FunctionKind::Closure),
            "block" | "do_block" if !is_lambda_body(node) => Some(FunctionKind::Closure),
            _ => None,
        }
    }

    fn is_class(&self, node: Node<'_>) -> bool {
        node.is_named() && matches!(node.kind(), "class" | "module")
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
        // `items.each { }` is named after the call it is passed to.
        let call = node.parent().filter(|p| p.kind() == "call");
        match call.and_then(|c| c.child_by_field_name("method")) {
            Some(method) => format!("{} block", text(method, source)),
            None => "<lambda>".into(),
        }
    }

    fn control(&self, node: Node<'_>, _source: &[u8]) -> Control {
        // Keyword tokens (`if`, `else`, `while`) share the kind name of the node they open.
        if !node.is_named() {
            return Control::None;
        }
        match node.kind() {
            "if" | "unless" => Control::If,
            "elsif" => Control::ElseIf,
            // `else` of `case` and `begin` is not a branch of an `if`.
            "else" if matches!(parent_kind(node), "if" | "unless" | "elsif") => Control::Else,
            "if_modifier" | "unless_modifier" | "while_modifier" | "until_modifier" => {
                Control::Flat
            }
            "while" | "until" | "for" => Control::Loop,
            "case" | "case_match" => Control::Switch,
            "when" | "in_clause" => Control::Case,
            "rescue" => Control::Catch,
            "conditional" => Control::Ternary,
            _ => Control::None,
        }
    }

    fn boolean_kinds(&self) -> &'static [&'static str] {
        &["binary"]
    }

    fn is_comment(&self, node: Node<'_>) -> bool {
        node.kind() == "comment"
    }

    fn is_block(&self, node: Node<'_>) -> bool {
        matches!(
            node.kind(),
            "body_statement" | "then" | "block_body" | "do" | "begin"
        )
    }

    fn params(&self, function: Node<'_>, _source: &[u8]) -> u32 {
        parameter_list(function).len() as u32
    }

    fn receiver(&self, method: Node<'_>, _source: &[u8]) -> Option<String> {
        (method.kind() == "method").then(|| "self".into())
    }

    fn member(&self, node: Node<'_>, source: &[u8], receiver: &str) -> Option<String> {
        match node.kind() {
            "instance_variable" => Some(text(node, source).to_owned()),
            "call" => {
                let object = node.child_by_field_name("receiver")?;
                if text(object, source) != receiver {
                    return None;
                }
                Some(text(node.child_by_field_name("method")?, source).to_owned())
            }
            _ => implicit_member(node, source, &GRAMMAR),
        }
    }

    fn is_negation(&self, node: Node<'_>) -> bool {
        node.kind() == "unary"
            && node
                .child_by_field_name("operator")
                .is_some_and(|o| matches!(o.kind(), "!" | "not"))
    }

    /// Ruby declares no parameter types.
    fn typed_params(&self, _function: Node<'_>, _source: &[u8]) -> (u32, u32) {
        (0, 0)
    }

    fn is_constructor(&self, function: Node<'_>, name: &str) -> bool {
        function.kind() == "method" && name == "initialize"
    }

    fn inherited_signature(&self, _function: Node<'_>, _source: &[u8]) -> bool {
        false
    }
}
