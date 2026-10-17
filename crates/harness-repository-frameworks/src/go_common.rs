//! What the Go web adapters share: router variables and groups, package-qualified names, and
//! finding the file that declares a handler.
use std::collections::HashMap;
use std::path::PathBuf;

use harness_workspace::{
    framework_relations::FrameworkInput,
    repository_intelligence::{Package, RootRelativePath},
};
use tree_sitter::Node;

use crate::syntax::{parse_go, text, walk};

pub fn go_files(package: &Package, input: &FrameworkInput<'_>) -> Vec<RootRelativePath> {
    input
        .files_of(package)
        .into_iter()
        .filter(|f| {
            f.path.as_path().extension().and_then(|e| e.to_str()) == Some("go")
                && !f.path.as_path().to_string_lossy().ends_with("_test.go")
        })
        .map(|f| f.path.clone())
        .collect()
}

/// The text of an interpreted or raw string literal.
pub fn string_value(node: Node<'_>, source: &str) -> Option<String> {
    matches!(
        node.kind(),
        "interpreted_string_literal" | "raw_string_literal"
    )
    .then(|| {
        let raw = text(node, source);
        raw.get(1..raw.len().saturating_sub(1))
            .unwrap_or("")
            .to_owned()
    })
}

pub fn join_path(prefix: &str, path: &str) -> String {
    let parts: Vec<&str> = [prefix, path]
        .iter()
        .flat_map(|p| p.split('/'))
        .filter(|p| !p.is_empty())
        .collect();
    let mut joined = format!("/{}", parts.join("/"));
    if path.ends_with('/') && !joined.ends_with('/') {
        joined.push('/');
    }
    joined
}

/// The call's arguments as nodes.
pub fn arguments<'t>(call: Node<'t>) -> Vec<Node<'t>> {
    let Some(list) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = list.walk();
    list.named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
        .collect()
}

/// `(operand, field)` of a call whose function is a selector: `r.GET(…)` gives (`r`, `GET`).
pub fn selector_call<'t>(call: Node<'t>, source: &str) -> Option<(Node<'t>, String)> {
    let function = call
        .child_by_field_name("function")
        .filter(|f| f.kind() == "selector_expression")?;
    let operand = function.child_by_field_name("operand")?;
    let field = function.child_by_field_name("field")?;
    Some((operand, text(field, source).to_owned()))
}

/// Variables holding a router (or a group of one), with the path prefix each adds:
/// `r := gin.Default()` gives `r` the prefix `""`, and `v1 := r.Group("/v1")` gives `v1` the prefix `/v1`.
pub fn router_variables(
    root: Node<'_>,
    source: &str,
    constructors: &[(&str, &str)],
    group_method: &str,
) -> HashMap<String, String> {
    let mut found: HashMap<String, String> = HashMap::new();
    walk(root, &mut |node| {
        if node.kind() != "short_var_declaration" && node.kind() != "assignment_statement" {
            return;
        }
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return;
        };
        let (Some(name), Some(call)) = (left.named_child(0), right.named_child(0)) else {
            return;
        };
        if name.kind() != "identifier" || call.kind() != "call_expression" {
            return;
        }
        let Some((operand, field)) = selector_call(call, source) else {
            return;
        };
        let operand_text = text(operand, source);
        if constructors
            .iter()
            .any(|(package, function)| *package == operand_text && *function == field)
        {
            found.insert(text(name, source).to_owned(), String::new());
        } else if field == group_method {
            if let Some(parent_prefix) = found.get(operand_text).cloned() {
                let part = arguments(call)
                    .first()
                    .and_then(|a| string_value(*a, source))
                    .unwrap_or_default();
                found.insert(
                    text(name, source).to_owned(),
                    join_path(&parent_prefix, &part),
                );
            }
        }
    });
    found
}

/// For one file: the package qualifier an import is used under (its alias, else the last path
/// segment) -> the file the import resolved to.
pub fn import_bindings(
    input: &FrameworkInput<'_>,
    file: &RootRelativePath,
) -> HashMap<String, RootRelativePath> {
    let mut found = HashMap::new();
    for edge in input
        .imports
        .edges
        .iter()
        .filter(|e| &e.fact.importing_file == file)
    {
        let Some(target) = edge.resolution.target_file() else {
            continue;
        };
        let binding = edge.fact.alias.clone().unwrap_or_else(|| {
            edge.fact
                .spelling
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_owned()
        });
        found.entry(binding).or_insert_with(|| target.clone());
    }
    found
}

/// Functions declared at the top level of the Go files in one directory, name -> file.
pub struct PackageFunctions {
    cache: HashMap<PathBuf, HashMap<String, RootRelativePath>>,
}

impl PackageFunctions {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    pub fn find(
        &mut self,
        input: &FrameworkInput<'_>,
        directory: &std::path::Path,
        name: &str,
    ) -> Option<RootRelativePath> {
        if !self.cache.contains_key(directory) {
            let mut map = HashMap::new();
            for indexed in &input.imports.files {
                if indexed.path.directory() != directory
                    || indexed.path.as_path().extension().and_then(|e| e.to_str()) != Some("go")
                {
                    continue;
                }
                let Some(source) = input.read(&indexed.path) else {
                    continue;
                };
                let Some(tree) = parse_go(&source) else {
                    continue;
                };
                let root = tree.root_node();
                let mut cursor = root.walk();
                for child in root.children(&mut cursor) {
                    if child.kind() == "function_declaration" {
                        if let Some(n) = child.child_by_field_name("name") {
                            map.entry(text(n, &source).to_owned())
                                .or_insert_with(|| indexed.path.clone());
                        }
                    }
                }
            }
            self.cache.insert(directory.to_path_buf(), map);
        }
        self.cache.get(directory).and_then(|m| m.get(name)).cloned()
    }
}

/// Resolves a handler expression: a function literal, a function of the same package, or `pkg.Func`.
pub fn resolve_handler(
    node: Node<'_>,
    file: &RootRelativePath,
    source: &str,
    input: &FrameworkInput<'_>,
    bindings: &HashMap<String, RootRelativePath>,
    functions: &mut PackageFunctions,
) -> (Option<RootRelativePath>, Option<String>) {
    match node.kind() {
        "func_literal" => (Some(file.clone()), None),
        "identifier" => {
            let name = text(node, source);
            (
                functions.find(input, file.directory(), name),
                Some(name.to_owned()),
            )
        }
        "selector_expression" => {
            let operand = node.child_by_field_name("operand").map(|o| text(o, source));
            let field = node
                .child_by_field_name("field")
                .map(|f| text(f, source).to_owned());
            let target = operand.and_then(|o| bindings.get(o).cloned());
            (target, field)
        }
        _ => (None, None),
    }
}
