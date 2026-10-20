//! What each language's syntax means for the measurements: which nodes are functions, classes,
//! branches and loops, how parameters and instance members are written.
use tree_sitter::{Language, Node};

use super::model::FunctionKind;
use crate::model::LanguageId;

mod cpp;
mod csharp;
mod go;
mod java;
mod javascript;
mod kotlin;
mod members;
mod php;
mod python;
mod ruby;
mod rust;

/// What a node contributes to complexity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    None,
    /// `if`: a branch that nests.
    If,
    /// `else if` / `elif`: a branch at the same depth as its `if`.
    ElseIf,
    Else,
    Loop,
    /// `switch` / `match`: nests; its arms add to CCN.
    Switch,
    /// A non-default `case` or match arm.
    Case,
    Catch,
    /// A conditional expression.
    Ternary,
    /// A branch that neither nests nor is a statement (a comprehension `if`).
    Flat,
}

pub trait Dialect {
    fn grammar(&self, path: &str) -> Language;
    /// Whether `node` is a function to measure, and what kind.
    fn function_kind(&self, node: Node<'_>) -> Option<FunctionKind>;
    fn is_class(&self, node: Node<'_>) -> bool;
    fn class_name(&self, node: Node<'_>, source: &[u8]) -> String;
    fn function_name(&self, node: Node<'_>, source: &[u8]) -> String;
    fn control(&self, node: Node<'_>, source: &[u8]) -> Control;
    /// Node kinds that combine operands with `&&` / `||` / `and` / `or` tokens.
    fn boolean_kinds(&self) -> &'static [&'static str];
    fn is_transparent(&self, node: Node<'_>) -> bool {
        node.kind() == "parenthesized_expression"
    }
    fn is_comment(&self, node: Node<'_>) -> bool;
    fn is_docstring(&self, _node: Node<'_>, _source: &[u8]) -> bool {
        false
    }
    /// Kinds of statement blocks, for children that carry no field name.
    fn is_block(&self, node: Node<'_>) -> bool;
    /// The node whose subtree is the function's body.
    fn body<'t>(&self, function: Node<'t>) -> Option<Node<'t>> {
        function.child_by_field_name("body")
    }
    /// The role of `child` in a control statement when the grammar gives it no field name
    /// (Kotlin's `if` branches), spelled like the fields other grammars use.
    fn implicit_field(&self, _statement: Node<'_>, _child: Node<'_>) -> Option<&'static str> {
        None
    }
    /// The type a method belongs to when it is written outside the type's own node: a Go
    /// receiver, C++'s `Type::method`.
    fn owner_hint(&self, _function: Node<'_>, _source: &[u8]) -> Option<String> {
        None
    }
    /// Whether methods named by `owner_hint` form a class when no node declares it.
    fn synthesizes_classes(&self) -> bool {
        false
    }
    fn params(&self, function: Node<'_>, source: &[u8]) -> u32;
    /// How a method refers to its instance (`self`, `this`, or Python's first parameter).
    fn receiver(&self, method: Node<'_>, source: &[u8]) -> Option<String>;
    /// The member name when `node` reads `receiver.name`.
    fn member(&self, node: Node<'_>, source: &[u8], receiver: &str) -> Option<String>;

    /// `!x` or `not x`.
    fn is_negation(&self, node: Node<'_>) -> bool;
    /// `(typed, primitive)`: parameters that declare a type, and those whose type is a scalar or
    /// a string.
    fn typed_params(&self, function: Node<'_>, source: &[u8]) -> (u32, u32);
    fn is_constructor(&self, function: Node<'_>, name: &str) -> bool;
    /// A trait implementation or an override, whose parameters another declaration decides.
    fn inherited_signature(&self, function: Node<'_>, source: &[u8]) -> bool;

    /// The boolean operators written directly in `node`.
    fn boolean_operators(&self, node: Node<'_>) -> Vec<&'static str> {
        if !self.boolean_kinds().contains(&node.kind()) {
            return Vec::new();
        }
        let mut cursor = node.walk();
        node.children(&mut cursor)
            .filter_map(|child| match child.kind() {
                "&&" => Some("&&"),
                "||" => Some("||"),
                "and" => Some("and"),
                "or" => Some("or"),
                _ => None,
            })
            .collect()
    }
}

pub fn for_language(language: &LanguageId) -> Option<Box<dyn Dialect>> {
    match language {
        LanguageId::Rust => Some(Box::new(rust::Rust)),
        LanguageId::TypeScript => Some(Box::new(javascript::Js { typescript: true })),
        LanguageId::JavaScript => Some(Box::new(javascript::Js { typescript: false })),
        LanguageId::Python => Some(Box::new(python::Python)),
        LanguageId::Java => Some(Box::new(java::Java)),
        LanguageId::Kotlin => Some(Box::new(kotlin::Kotlin)),
        LanguageId::Go => Some(Box::new(go::Go)),
        LanguageId::CSharp => Some(Box::new(csharp::CSharp)),
        LanguageId::Php => Some(Box::new(php::Php)),
        LanguageId::Cpp => Some(Box::new(cpp::Cpp)),
        LanguageId::Ruby => Some(Box::new(ruby::Ruby)),
    }
}

pub fn text<'a>(node: Node<'_>, source: &'a [u8]) -> &'a str {
    std::str::from_utf8(&source[node.byte_range()]).unwrap_or("")
}

/// Named children of `node` that are not comments.
pub fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| !c.kind().contains("comment"))
        .collect()
}

/// The name an anonymous function gets from where it stands: the variable or property it is
/// assigned to, or the call it is passed to.
pub fn name_from_context(node: Node<'_>, source: &[u8], fallback: &str) -> String {
    let Some(parent) = node.parent() else {
        return fallback.to_owned();
    };
    for field in ["name", "left", "key", "pattern", "property"] {
        if let Some(target) = parent.child_by_field_name(field) {
            if target.id() != node.id() {
                return text(target, source).to_owned();
            }
        }
    }
    if matches!(parent.kind(), "arguments" | "argument_list") {
        if let Some(call) = parent.parent() {
            if let Some(function) = call.child_by_field_name("function") {
                let callee = text(function, source);
                let short = callee.rsplit(['.', ':']).next().unwrap_or(callee);
                return format!("{short} callback");
            }
        }
    }
    fallback.to_owned()
}
