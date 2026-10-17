//! Route registration shared by the Go routers that look alike (Gin and Echo): a variable created
//! by a known constructor, groups that add a path prefix, and `GET("/path", handler)` calls.
use harness_workspace::{
    framework_relations::{
        Certainty, FrameworkInput, FrameworkOutput, FrameworkRelation, RelationDraft, RelationKind,
    },
    repository_intelligence::{DiscoveryError, Package},
};
use tokio_util::sync::CancellationToken;

use crate::{
    go_common::{
        arguments, go_files, import_bindings, join_path, resolve_handler, router_variables,
        selector_call, string_value, PackageFunctions,
    },
    syntax::{evidence, parse_go, text, walk},
};

/// Where the handler sits among a registration call's arguments.
#[derive(Clone, Copy)]
pub enum HandlerAt {
    /// After any middleware: Gin.
    Last,
    /// Right after the path: Echo (middleware follows it).
    Second,
}

pub struct Dialect {
    pub framework: &'static str,
    pub rule: &'static str,
    pub version: u32,
    /// A text that must appear in the file for it to be read (the import path).
    pub marker: &'static str,
    pub constructors: &'static [(&'static str, &'static str)],
    pub methods: &'static [&'static str],
    pub handler: HandlerAt,
}

pub fn extract(
    dialect: &Dialect,
    package: &Package,
    input: &FrameworkInput<'_>,
    cancel: &CancellationToken,
    output: &mut FrameworkOutput,
) -> Result<(), DiscoveryError> {
    let mut functions = PackageFunctions::new();
    for file in go_files(package, input) {
        if cancel.is_cancelled() {
            return Err(DiscoveryError::Cancelled);
        }
        let Some(source) = input.read(&file) else {
            continue;
        };
        if !source.contains(dialect.marker) {
            continue;
        }
        let Some(tree) = parse_go(&source) else {
            continue;
        };
        let root = tree.root_node();
        let receivers = router_variables(root, &source, dialect.constructors, "Group");
        if receivers.is_empty() {
            continue;
        }
        let bindings = import_bindings(input, &file);
        walk(root, &mut |node| {
            if node.kind() != "call_expression" {
                return;
            }
            let Some((operand, method)) = selector_call(node, &source) else {
                return;
            };
            if !dialect.methods.contains(&method.as_str()) {
                return;
            }
            let Some(prefix) = receivers.get(text(operand, &source)) else {
                return;
            };
            let args = arguments(node);
            let (Some(path_node), true) = (args.first(), args.len() >= 2) else {
                return;
            };
            let Some(path) = string_value(*path_node, &source) else {
                output.unresolved(
                    &file,
                    format!(
                        "`{}.{method}({}, …)` has a computed path; no relation emitted",
                        text(operand, &source),
                        text(*path_node, &source)
                    ),
                );
                return;
            };
            let handler = match dialect.handler {
                HandlerAt::Last => args.last(),
                HandlerAt::Second => args.get(1),
            };
            let (target, symbol) = handler.map_or((None, None), |h| {
                resolve_handler(*h, &file, &source, input, &bindings, &mut functions)
            });
            output.relate(FrameworkRelation::new(
                dialect.framework,
                Some(&package.id),
                RelationDraft {
                    kind: RelationKind::RouteRegisteredTo,
                    rule: dialect.rule,
                    rule_version: dialect.version,
                    certainty: Certainty::Declared,
                    from: file.clone(),
                    to: target,
                    from_symbol: None,
                    to_symbol: symbol,
                    label: format!("{} {}", method.to_uppercase(), join_path(prefix, &path)),
                    evidence: evidence(&file, node, &source),
                },
            ));
        });
    }
    Ok(())
}
