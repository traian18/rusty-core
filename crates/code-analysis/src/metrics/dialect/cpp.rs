use tree_sitter::{Language, Node};

use super::members::{children_of, implicit_member, Grammar};
use super::{name_from_context, named_children, text, Control, Dialect};
use crate::metrics::model::FunctionKind;

pub struct Cpp;

const PRIMITIVES: [&str; 14] = [
    "int", "long", "short", "char", "float", "double", "bool", "unsigned", "signed", "size_t",
    "int32_t", "int64_t", "uint32_t", "uint64_t",
];

/// The `function_declarator` that carries a definition's name and parameters, found under any
/// pointer or reference declarators.
fn function_declarator(function: Node<'_>) -> Option<Node<'_>> {
    let mut node = function.child_by_field_name("declarator")?;
    while node.kind() != "function_declarator" && node.kind() != "abstract_function_declarator" {
        node = node.child_by_field_name("declarator")?;
    }
    Some(node)
}

fn parameters<'t>(function: Node<'t>) -> Vec<Node<'t>> {
    let Some(list) =
        function_declarator(function).and_then(|d| d.child_by_field_name("parameters"))
    else {
        return Vec::new();
    };
    named_children(list)
        .into_iter()
        .filter(|p| {
            matches!(
                p.kind(),
                "parameter_declaration"
                    | "optional_parameter_declaration"
                    | "variadic_parameter_declaration"
            )
        })
        .collect()
}

/// The written name, such as `Counter::bump`, `~Counter` or `operator+`.
fn written_name<'a>(function: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    let declarator = function_declarator(function)?;
    let name = declarator.child_by_field_name("declarator")?;
    Some(text(name, source))
}

fn has_storage(function: Node<'_>, source: &[u8], word: &str) -> bool {
    children_of(function, "storage_class_specifier")
        .iter()
        .any(|s| text(*s, source).trim() == word)
}

/// The name a declarator introduces, under any pointer, reference, array or initializer wrapper.
fn declarator_name(declarator: Node<'_>, source: &[u8]) -> Option<String> {
    let mut node = declarator;
    loop {
        match node.kind() {
            "identifier" | "field_identifier" => return Some(text(node, source).to_owned()),
            "qualified_identifier" | "destructor_name" | "operator_name" => return None,
            _ => {
                node = node
                    .child_by_field_name("declarator")
                    .or_else(|| node.named_child(0))?
            }
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
        "function_declarator"
        | "init_declarator"
        | "declaration"
        | "field_declaration"
        | "parameter_declaration"
        | "optional_parameter_declaration"
        | "for_range_loop" => !is_field("declarator"),
        "pointer_declarator"
        | "reference_declarator"
        | "array_declarator"
        | "qualified_identifier"
        | "namespace_definition"
        | "using_declaration"
        | "labeled_statement"
        | "goto_statement" => false,
        _ => true,
    }
}

fn scope_names(scope: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    match scope.kind() {
        "function_definition" | "lambda_expression" => {
            for param in parameters(scope) {
                if let Some(name) = param
                    .child_by_field_name("declarator")
                    .and_then(|d| declarator_name(d, source))
                {
                    out.push(name);
                }
            }
        }
        "compound_statement" => {
            for local in children_of(scope, "declaration") {
                let mut cursor = local.walk();
                for declarator in local.children_by_field_name("declarator", &mut cursor) {
                    out.extend(declarator_name(declarator, source));
                }
            }
        }
        "for_statement" => {
            if let Some(init) = scope.child_by_field_name("initializer") {
                let mut cursor = init.walk();
                for declarator in init.children_by_field_name("declarator", &mut cursor) {
                    out.extend(declarator_name(declarator, source));
                }
            }
        }
        "for_range_loop" => {
            if let Some(declarator) = scope.child_by_field_name("declarator") {
                out.extend(declarator_name(declarator, source));
            }
        }
        "catch_clause" => {
            if let Some(list) = scope.child_by_field_name("parameters") {
                for param in children_of(list, "parameter_declaration") {
                    if let Some(declarator) = param.child_by_field_name("declarator") {
                        out.extend(declarator_name(declarator, source));
                    }
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
                let mut cursor = member.walk();
                for declarator in member.children_by_field_name("declarator", &mut cursor) {
                    out.extend(declarator_name(declarator, source));
                }
            }
            "function_definition" => {
                if let Some(name) = written_name(member, source) {
                    out.push(name.to_owned());
                }
            }
            _ => {}
        }
    }
}

const GRAMMAR: Grammar = Grammar {
    class_bodies: &["field_declaration_list"],
    is_reference,
    scope_names,
    member_names,
};

impl Dialect for Cpp {
    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_cpp::LANGUAGE.into()
    }

    fn function_kind(&self, node: Node<'_>) -> Option<FunctionKind> {
        if !node.is_named() {
            return None;
        }
        match node.kind() {
            "function_definition" => {
                let in_class = node
                    .parent()
                    .is_some_and(|p| p.kind() == "field_declaration_list");
                let qualified = function_declarator(node)
                    .and_then(|d| d.child_by_field_name("declarator"))
                    .is_some_and(|n| n.kind() == "qualified_identifier");
                Some(if in_class || qualified {
                    FunctionKind::Method
                } else {
                    FunctionKind::Function
                })
            }
            "lambda_expression" => Some(FunctionKind::Closure),
            _ => None,
        }
    }

    fn is_class(&self, node: Node<'_>) -> bool {
        node.is_named()
            && matches!(node.kind(), "class_specifier" | "struct_specifier")
            && node.child_by_field_name("body").is_some()
    }

    fn class_name(&self, node: Node<'_>, source: &[u8]) -> String {
        node.child_by_field_name("name")
            .map(|n| text(n, source).to_owned())
            .unwrap_or_else(|| "<class>".into())
    }

    fn function_name(&self, node: Node<'_>, source: &[u8]) -> String {
        if node.kind() == "lambda_expression" {
            return name_from_context(node, source, "<lambda>");
        }
        match written_name(node, source) {
            Some(name) => name.rsplit("::").next().unwrap_or(name).to_owned(),
            None => "<function>".into(),
        }
    }

    fn owner_hint(&self, function: Node<'_>, source: &[u8]) -> Option<String> {
        let name = function_declarator(function)?.child_by_field_name("declarator")?;
        if name.kind() != "qualified_identifier" {
            return None;
        }
        let scope = text(name.child_by_field_name("scope")?, source);
        let last = scope.rsplit("::").next().unwrap_or(scope);
        Some(last.split('<').next().unwrap_or(last).trim().to_owned())
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
            "for_statement" | "for_range_loop" | "while_statement" | "do_statement" => {
                Control::Loop
            }
            "switch_statement" => Control::Switch,
            "case_statement" => {
                if node.child_by_field_name("value").is_some() {
                    Control::Case
                } else {
                    Control::None
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
        node.kind() == "compound_statement"
    }

    fn params(&self, function: Node<'_>, source: &[u8]) -> u32 {
        let params = parameters(function);
        // `f(void)` takes nothing.
        if let [only] = params.as_slice() {
            if only.child_by_field_name("declarator").is_none()
                && only
                    .child_by_field_name("type")
                    .is_some_and(|t| text(t, source).trim() == "void")
            {
                return 0;
            }
        }
        params.len() as u32
    }

    fn receiver(&self, method: Node<'_>, source: &[u8]) -> Option<String> {
        (method.kind() == "function_definition" && !has_storage(method, source, "static"))
            .then(|| "this".into())
    }

    fn member(&self, node: Node<'_>, source: &[u8], receiver: &str) -> Option<String> {
        if node.kind() != "field_expression" {
            return implicit_member(node, source, &GRAMMAR);
        }
        let object = node.child_by_field_name("argument")?;
        if text(object, source) != receiver {
            return None;
        }
        Some(text(node.child_by_field_name("field")?, source).to_owned())
    }

    fn is_negation(&self, node: Node<'_>) -> bool {
        node.kind() == "unary_expression"
            && node
                .child_by_field_name("operator")
                .is_some_and(|o| matches!(o.kind(), "!" | "not"))
    }

    fn typed_params(&self, function: Node<'_>, source: &[u8]) -> (u32, u32) {
        let mut typed = 0;
        let mut primitive = 0;
        for param in parameters(function) {
            let Some(ty) = param.child_by_field_name("type") else {
                continue;
            };
            typed += 1;
            // A pointer or reference to an int is not a scalar argument.
            let by_value = param
                .child_by_field_name("declarator")
                .is_none_or(|d| d.kind() == "identifier");
            let words = text(ty, source).split_whitespace().collect::<Vec<_>>();
            let scalar = !words.is_empty() && words.iter().all(|w| PRIMITIVES.contains(w))
                || text(ty, source).trim() == "std::string";
            primitive += u32::from(by_value && scalar);
        }
        (typed, primitive)
    }

    /// A definition with no return type that is not a destructor or conversion operator.
    fn is_constructor(&self, function: Node<'_>, _name: &str) -> bool {
        if function.child_by_field_name("type").is_some() {
            return false;
        }
        let Some(mut name) =
            function_declarator(function).and_then(|d| d.child_by_field_name("declarator"))
        else {
            return false;
        };
        if name.kind() == "qualified_identifier" {
            let Some(inner) = name.child_by_field_name("name") else {
                return false;
            };
            name = inner;
        }
        !matches!(
            name.kind(),
            "destructor_name" | "operator_name" | "operator_cast"
        )
    }

    fn inherited_signature(&self, function: Node<'_>, source: &[u8]) -> bool {
        function_declarator(function).is_some_and(|d| {
            children_of(d, "virtual_specifier")
                .iter()
                .any(|v| matches!(text(*v, source).trim(), "override" | "final"))
        })
    }
}
