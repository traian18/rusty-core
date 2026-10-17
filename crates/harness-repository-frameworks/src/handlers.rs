//! Following the function a route names to the file that defines it.
use std::collections::HashSet;

use harness_workspace::{
    framework_relations::FrameworkInput, repository_intelligence::RootRelativePath,
};
use tree_sitter::Node;

use crate::syntax::text;

/// Names declared at the top level of a script file: functions, classes and variables, exported or not.
pub fn top_level_names(root: Node<'_>, source: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        let declaration = if child.kind() == "export_statement" {
            child.child_by_field_name("declaration")
        } else {
            Some(child)
        };
        let Some(declaration) = declaration else {
            continue;
        };
        match declaration.kind() {
            "function_declaration" | "generator_function_declaration" | "class_declaration" => {
                if let Some(name) = declaration.child_by_field_name("name") {
                    names.insert(text(name, source).to_owned());
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                let mut inner = declaration.walk();
                for declarator in declaration.children(&mut inner) {
                    if declarator.kind() == "variable_declarator" {
                        if let Some(name) = declarator.child_by_field_name("name") {
                            if name.kind() == "identifier" {
                                names.insert(text(name, source).to_owned());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    names
}

/// Where a handler expression lives.
pub struct Handler {
    pub file: Option<RootRelativePath>,
    pub symbol: Option<String>,
}

/// Resolves the expression a route hands to the framework:
/// - an inline function belongs to the file that registers the route;
/// - an identifier declared in the file is that file's, one imported resolves through the import index;
/// - `controller.method` resolves `controller` the same way and names the method;
/// - anything else (a call, a computed value) is not followed, and has no file.
pub fn resolve_handler(
    node: Node<'_>,
    file: &RootRelativePath,
    source: &str,
    locals: &HashSet<String>,
    input: &FrameworkInput<'_>,
) -> Handler {
    let named = |name: &str, symbol: Option<String>| Handler {
        file: if locals.contains(name) {
            Some(file.clone())
        } else {
            input.names.target(file, name).cloned()
        },
        symbol,
    };
    match node.kind() {
        "arrow_function" | "function_expression" | "function" | "generator_function" => Handler {
            file: Some(file.clone()),
            symbol: None,
        },
        "identifier" => {
            let name = text(node, source);
            named(name, Some(name.to_owned()))
        }
        "member_expression" => {
            let object = node.child_by_field_name("object");
            let property = node.child_by_field_name("property");
            match (object, property) {
                (Some(object), Some(property)) if object.kind() == "identifier" => named(
                    text(object, source),
                    Some(text(property, source).to_owned()),
                ),
                _ => Handler {
                    file: None,
                    symbol: None,
                },
            }
        }
        _ => Handler {
            file: None,
            symbol: None,
        },
    }
}

/// Whether `name` is a parameter of a function that encloses `node`, shadowing any outer variable.
pub fn is_parameter(node: Node<'_>, name: &str, source: &str) -> bool {
    let mut current = node.parent();
    while let Some(ancestor) = current {
        if matches!(
            ancestor.kind(),
            "function_declaration"
                | "function_expression"
                | "function"
                | "arrow_function"
                | "method_definition"
                | "generator_function"
                | "generator_function_declaration"
        ) {
            if let Some(parameters) = ancestor
                .child_by_field_name("parameters")
                .or_else(|| ancestor.child_by_field_name("parameter"))
            {
                let mut found = false;
                crate::syntax::walk(parameters, &mut |n| {
                    if n.kind() == "identifier" && text(n, source) == name {
                        found = true;
                    }
                });
                if found {
                    return true;
                }
            }
        }
        current = ancestor.parent();
    }
    false
}
