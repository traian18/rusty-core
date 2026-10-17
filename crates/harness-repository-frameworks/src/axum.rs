//! Axum: routes written as `.route("/path", get(handler).post(other))`.
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
    rust_items::{local_functions, resolve_path},
    syntax::{method_call_evidence, parse_rust, string_value, text, walk},
};

const ID: &str = "axum";
const VERSION: u32 = 1;
const RULE: &str = "axum-route";
const METHODS: [&str; 9] = [
    "get", "post", "put", "delete", "patch", "head", "options", "trace", "any",
];

pub struct AxumAdapter;

impl FrameworkAdapter for AxumAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "Axum".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::RouteRegisteredTo],
            handled: vec![
                ".route(\"/literal\", get(handler)) with chained .post(…), .put(…) and the other method routers, in files that use axum".into(),
                "handlers: a function in the file, a function imported with use, module::function, or a closure".into(),
            ],
            unsupported: vec![
                ".nest prefixes and .merge are not combined; a route is labelled with the path written on it".into(),
                "route_service, on(MethodFilter, …), get_service and routes built from computed strings (reported, no relation)".into(),
                "state, layers and extractors".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        declares(package, "axum")
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
            if indexed.path.as_path().extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let Some(source) = input.read(&indexed.path) else {
                continue;
            };
            if !source.contains("axum") || !source.contains(".route(") {
                continue;
            }
            let Some(tree) = parse_rust(&source) else {
                continue;
            };
            let file = &indexed.path;
            let locals = local_functions(tree.root_node(), &source);
            walk(tree.root_node(), &mut |node| {
                if node.kind() != "call_expression" {
                    return;
                }
                let Some(function) = node
                    .child_by_field_name("function")
                    .filter(|f| f.kind() == "field_expression")
                else {
                    return;
                };
                if function
                    .child_by_field_name("field")
                    .map(|f| text(f, &source))
                    != Some("route")
                {
                    return;
                }
                read_route(package, file, node, &source, &locals, input, output);
            });
        }
        Ok(())
    }
}

fn read_route(
    package: &Package,
    file: &RootRelativePath,
    call: Node<'_>,
    source: &str,
    locals: &std::collections::HashSet<String>,
    input: &FrameworkInput<'_>,
    output: &mut FrameworkOutput,
) {
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return;
    };
    let mut cursor = arguments.walk();
    let args: Vec<Node<'_>> = arguments.named_children(&mut cursor).collect();
    let [path_node, router, ..] = args.as_slice() else {
        return;
    };
    let Some(path) = string_value(*path_node, source) else {
        output.unresolved(
            file,
            format!(
                "`.route({}, …)` has a computed path; no relation emitted",
                text(*path_node, source)
            ),
        );
        return;
    };
    let mut handlers = Vec::new();
    if !method_router(*router, source, &mut handlers) {
        output.unresolved(
            file,
            format!(
                "`.route(\"{path}\", {})` is not a plain method router; no relation emitted",
                text(*router, source)
            ),
        );
        return;
    }
    for (method, handler) in handlers {
        let (target, symbol) = resolve_path(handler, file, source, locals, input);
        output.relate(FrameworkRelation::new(
            ID,
            Some(&package.id),
            RelationDraft {
                kind: RelationKind::RouteRegisteredTo,
                rule: RULE,
                rule_version: VERSION,
                certainty: Certainty::Declared,
                from: file.clone(),
                to: target,
                from_symbol: None,
                to_symbol: symbol,
                label: format!("{} {path}", method.to_uppercase()),
                evidence: method_call_evidence(file, call, source),
            },
        ));
    }
}

/// Reads `get(h)`, `get(h).post(h2)` and `routing::get(h)`; false for any other expression.
fn method_router<'t>(node: Node<'t>, source: &str, out: &mut Vec<(String, Node<'t>)>) -> bool {
    if node.kind() != "call_expression" {
        return false;
    }
    let Some(function) = node.child_by_field_name("function") else {
        return false;
    };
    let handler = node
        .child_by_field_name("arguments")
        .and_then(|a| a.named_child(0));
    match function.kind() {
        "field_expression" => {
            let (Some(inner), Some(field)) = (
                function.child_by_field_name("value"),
                function.child_by_field_name("field"),
            ) else {
                return false;
            };
            let method = text(field, source);
            let Some(handler) = handler.filter(|_| METHODS.contains(&method)) else {
                return false;
            };
            out.push((method.to_owned(), handler));
            method_router(inner, source, out)
        }
        "identifier" | "scoped_identifier" => {
            let method = text(function, source)
                .rsplit("::")
                .next()
                .unwrap_or_default();
            let Some(handler) = handler.filter(|_| METHODS.contains(&method)) else {
                return false;
            };
            out.push((method.to_owned(), handler));
            true
        }
        _ => false,
    }
}
