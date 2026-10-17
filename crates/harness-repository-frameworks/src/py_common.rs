//! What the Python web adapters share: reading files, local names, and resolving a handler.
use std::collections::HashSet;

use harness_workspace::{
    framework_relations::FrameworkInput,
    repository_intelligence::{Package, RootRelativePath},
};
use tree_sitter::Node;

use crate::{py_items::functions, syntax::text};

/// The package's Python files in path order.
pub fn python_files(package: &Package, input: &FrameworkInput<'_>) -> Vec<RootRelativePath> {
    input
        .files_of(package)
        .into_iter()
        .filter(|f| f.path.as_path().extension().and_then(|e| e.to_str()) == Some("py"))
        .map(|f| f.path.clone())
        .collect()
}

/// Names of functions and classes defined at any level of the file (decorated or not).
pub fn local_names(root: Node<'_>, source: &str) -> HashSet<String> {
    let mut names: HashSet<String> = functions(root, source)
        .into_iter()
        .map(|f| f.name)
        .collect();
    crate::syntax::walk(root, &mut |n| {
        if n.kind() == "class_definition" {
            if let Some(name) = n.child_by_field_name("name") {
                names.insert(text(name, source).to_owned());
            }
        }
    });
    names
}

/// The file an identifier names: defined in this file, or imported from another file of the repository.
pub fn file_of_name(
    name: &str,
    file: &RootRelativePath,
    locals: &HashSet<String>,
    input: &FrameworkInput<'_>,
) -> Option<RootRelativePath> {
    if locals.contains(name) {
        Some(file.clone())
    } else {
        input.names.target(file, name).cloned()
    }
}

/// Resolves a view or handler expression: `name`, `module.name`, or `Class.as_view()`.
/// Returns the file and the name of the function or class.
pub fn resolve_view(
    node: Node<'_>,
    file: &RootRelativePath,
    source: &str,
    locals: &HashSet<String>,
    input: &FrameworkInput<'_>,
) -> (Option<RootRelativePath>, Option<String>) {
    match node.kind() {
        "identifier" => {
            let name = text(node, source);
            (
                file_of_name(name, file, locals, input),
                Some(name.to_owned()),
            )
        }
        "attribute" => {
            let object = node.child_by_field_name("object");
            let attribute = node
                .child_by_field_name("attribute")
                .map(|a| text(a, source).to_owned());
            // `views.list_users`: the module `views` was imported, so its file holds the view.
            let target = object
                .filter(|o| o.kind() == "identifier")
                .and_then(|o| file_of_name(text(o, source), file, locals, input))
                .or_else(|| {
                    attribute
                        .as_deref()
                        .and_then(|a| input.names.target(file, a).cloned())
                });
            (target, attribute)
        }
        "call" => {
            // `UserView.as_view()` and `Class.as_view(arg=1)`
            let function = node.child_by_field_name("function");
            match function.filter(|f| f.kind() == "attribute") {
                Some(function)
                    if function
                        .child_by_field_name("attribute")
                        .map(|a| text(a, source))
                        == Some("as_view") =>
                {
                    match function.child_by_field_name("object") {
                        Some(object) => resolve_view(object, file, source, locals, input),
                        None => (None, None),
                    }
                }
                _ => (None, None),
            }
        }
        "lambda" => (Some(file.clone()), None),
        _ => (None, None),
    }
}
