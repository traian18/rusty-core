//! Express: routes declared with `app.get('/path', handler)` and `router.post(...)`.
use std::collections::HashSet;

use harness_workspace::{
    framework_relations::{
        declares, Certainty, FrameworkAdapter, FrameworkCapability, FrameworkInput,
        FrameworkOutput, FrameworkRelation, RelationDraft, RelationKind,
    },
    repository_intelligence::{DiscoveryError, Package, RootRelativePath},
};
use tokio_util::sync::CancellationToken;
use tree_sitter::Node;

use crate::{
    handlers::{is_parameter, resolve_handler, top_level_names},
    syntax::{evidence, is_script, parse_script, string_value, text, walk},
};

const ID: &str = "express";
const VERSION: u32 = 1;
const RULE: &str = "express-route";
const METHODS: [&str; 8] = [
    "get", "post", "put", "delete", "patch", "options", "head", "all",
];

pub struct ExpressAdapter;

impl FrameworkAdapter for ExpressAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "Express".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::RouteRegisteredTo],
            handled: vec![
                "app.get/post/put/delete/patch/options/head/all('/literal', …, handler) on a variable created by express() or express.Router() in the same file".into(),
                "an array of literal paths gives one route per path".into(),
                "handlers: an inline function, a function declared in the file, an imported function, or imported object.method".into(),
            ],
            unsupported: vec![
                "mount prefixes (app.use('/api', router)) are not combined; a route is labelled with the path written on it".into(),
                "app.route('/x').get(…).post(…) chains".into(),
                "routers received as function parameters or imported from other files as the receiver".into(),
                "computed, template or regular-expression paths (reported, no relation)".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        declares(package, "express")
    }

    fn extract(
        &self,
        package: &Package,
        input: &FrameworkInput<'_>,
        cancel: &CancellationToken,
        output: &mut FrameworkOutput,
    ) -> Result<(), DiscoveryError> {
        for indexed in input.files_of(package) {
            if cancel.is_cancelled() {
                return Err(DiscoveryError::Cancelled);
            }
            if !is_script(&indexed.path) {
                continue;
            }
            let Some(source) = input.read(&indexed.path) else {
                continue;
            };
            if !source.contains("express") {
                continue;
            }
            let Some(tree) = parse_script(&indexed.path, &source) else {
                continue;
            };
            read_file(
                package,
                &indexed.path,
                &source,
                tree.root_node(),
                input,
                output,
            );
        }
        Ok(())
    }
}

/// Identifiers created by `express()`, `express.Router()` or `Router()` in this file.
fn routers(root: Node<'_>, source: &str) -> HashSet<String> {
    let mut found = HashSet::new();
    walk(root, &mut |node| {
        if node.kind() != "variable_declarator" {
            return;
        }
        let (Some(name), Some(value)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("value"),
        ) else {
            return;
        };
        if name.kind() != "identifier" || value.kind() != "call_expression" {
            return;
        }
        let Some(function) = value.child_by_field_name("function") else {
            return;
        };
        let callee = text(function, source);
        if matches!(callee, "express" | "express.Router" | "Router") {
            found.insert(text(name, source).to_owned());
        }
    });
    found
}

fn read_file(
    package: &Package,
    file: &RootRelativePath,
    source: &str,
    root: Node<'_>,
    input: &FrameworkInput<'_>,
    output: &mut FrameworkOutput,
) {
    let known = routers(root, source);
    let locals = top_level_names(root, source);
    walk(root, &mut |node| {
        if node.kind() != "call_expression" {
            return;
        }
        let Some(function) = node.child_by_field_name("function") else {
            return;
        };
        if function.kind() != "member_expression" {
            return;
        }
        let (Some(object), Some(property)) = (
            function.child_by_field_name("object"),
            function.child_by_field_name("property"),
        ) else {
            return;
        };
        let method = text(property, source);
        if !METHODS.contains(&method) || object.kind() != "identifier" {
            return;
        }
        let Some(arguments) = node.child_by_field_name("arguments") else {
            return;
        };
        let mut cursor = arguments.walk();
        let args: Vec<Node<'_>> = arguments.named_children(&mut cursor).collect();
        // `app.get('name')` reads a setting; a route needs a path and at least one handler.
        if args.len() < 2 {
            return;
        }
        let receiver = text(object, source);
        let path_node = args[0];
        let looks_like_route = string_value(path_node, source).is_some_and(|p| p.starts_with('/'))
            || path_node.kind() == "array";
        if !known.contains(receiver) || is_parameter(node, receiver, source) {
            if looks_like_route {
                output.unresolved(
                    file,
                    format!("`{receiver}.{method}({}, …)` looks like a route, but `{receiver}` is not provably an Express app or router; no relation emitted", text(path_node, source)),
                );
            }
            return;
        }
        let paths = match path_node.kind() {
            "array" => {
                let mut inner = path_node.walk();
                let elements: Vec<Node<'_>> = path_node.named_children(&mut inner).collect();
                let literals: Vec<String> = elements
                    .iter()
                    .filter_map(|e| string_value(*e, source))
                    .collect();
                if literals.len() != elements.len() {
                    output.unresolved(file, format!("`{receiver}.{method}({}, …)` has a path list with computed entries; no relation emitted", text(path_node, source)));
                    return;
                }
                literals
            }
            _ => {
                match string_value(path_node, source) {
                    Some(path) => vec![path],
                    None => {
                        output.unresolved(file, format!("`{receiver}.{method}({}, …)` has a computed path; no relation emitted", text(path_node, source)));
                        return;
                    }
                }
            }
        };
        let handler = resolve_handler(*args.last().unwrap(), file, source, &locals, input);
        for path in paths {
            output.relate(FrameworkRelation::new(
                ID,
                Some(&package.id),
                RelationDraft {
                    kind: RelationKind::RouteRegisteredTo,
                    rule: RULE,
                    rule_version: VERSION,
                    certainty: Certainty::Declared,
                    from: file.clone(),
                    to: handler.file.clone(),
                    from_symbol: None,
                    to_symbol: handler.symbol.clone(),
                    label: format!("{} {path}", method.to_uppercase()),
                    evidence: evidence(file, node, source),
                },
            ));
        }
    });
}
