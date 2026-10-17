//! Go's standard library: `http.HandleFunc("/path", handler)` and the same calls on a
//! `http.NewServeMux()` variable. Go 1.22 patterns ("GET /path") keep their method.
use std::collections::HashSet;

use harness_workspace::{
    framework_relations::{
        Certainty, FrameworkAdapter, FrameworkCapability, FrameworkInput, FrameworkOutput,
        FrameworkRelation, RelationDraft, RelationKind,
    },
    repository_intelligence::{DiscoveryError, Package},
};
use tokio_util::sync::CancellationToken;

use crate::{
    go_common::{
        arguments, go_files, import_bindings, resolve_handler, selector_call, string_value,
        PackageFunctions,
    },
    syntax::{evidence, parse_go, text, walk},
};

const ID: &str = "nethttp";
const VERSION: u32 = 1;
const RULE: &str = "nethttp-handle";
const METHODS: [&str; 2] = ["HandleFunc", "Handle"];

pub struct NetHttpAdapter;

impl FrameworkAdapter for NetHttpAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "net/http".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::RouteRegisteredTo],
            handled: vec![
                "http.HandleFunc and http.Handle, and the same methods on a variable created by http.NewServeMux(), with a literal pattern, in files that import net/http".into(),
                "patterns with a method (\"GET /items/{id}\") keep it; others are labelled ANY".into(),
            ],
            unsupported: vec![
                "third-party routers, handlers wrapped by middleware calls, and muxes passed as parameters".into(),
                "computed patterns (reported, no relation)".into(),
            ],
        }
    }

    /// The standard library needs no declaration, so any Go module applies; a file must still import
    /// `net/http` before anything is read from it.
    fn applicability(&self, package: &Package) -> Option<String> {
        (package
            .manifest
            .as_path()
            .file_name()
            .is_some_and(|n| n == "go.mod"))
        .then(|| "a Go module (net/http is part of the standard library)".to_owned())
    }

    fn extract(
        &self,
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
            if !source.contains("\"net/http\"") {
                continue;
            }
            let Some(tree) = parse_go(&source) else {
                continue;
            };
            let root = tree.root_node();
            let mut muxes: HashSet<String> = HashSet::from(["http".to_owned()]);
            walk(root, &mut |node| {
                if node.kind() != "short_var_declaration"
                    && node.kind() != "var_spec"
                    && node.kind() != "assignment_statement"
                {
                    return;
                }
                let (left, right) = (
                    node.child_by_field_name("left")
                        .or_else(|| node.child_by_field_name("name")),
                    node.child_by_field_name("right")
                        .or_else(|| node.child_by_field_name("value")),
                );
                let (Some(name), Some(call)) = (
                    left.and_then(|l| l.named_child(0).or(Some(l))),
                    right.and_then(|r| r.named_child(0).or(Some(r))),
                ) else {
                    return;
                };
                if call.kind() == "call_expression"
                    && selector_call(call, &source).is_some_and(|(operand, field)| {
                        text(operand, &source) == "http" && field == "NewServeMux"
                    })
                {
                    muxes.insert(text(name, &source).to_owned());
                }
            });
            let bindings = import_bindings(input, &file);
            walk(root, &mut |node| {
                if node.kind() != "call_expression" {
                    return;
                }
                let Some((operand, method)) = selector_call(node, &source) else {
                    return;
                };
                if !METHODS.contains(&method.as_str()) || !muxes.contains(text(operand, &source)) {
                    return;
                }
                let args = arguments(node);
                let [pattern_node, handler, ..] = args.as_slice() else {
                    return;
                };
                let Some(pattern) = string_value(*pattern_node, &source) else {
                    output.unresolved(
                        &file,
                        format!(
                            "`{}.{method}({}, …)` has a computed pattern; no relation emitted",
                            text(operand, &source),
                            text(*pattern_node, &source)
                        ),
                    );
                    return;
                };
                let label = match pattern.split_once(' ') {
                    Some((verb, path)) if verb.chars().all(|c| c.is_ascii_uppercase()) => {
                        format!("{verb} {}", path.trim())
                    }
                    _ => format!("ANY {pattern}"),
                };
                // `http.HandlerFunc(fn)` wraps a function; follow the function.
                let handler = if handler.kind() == "call_expression"
                    && selector_call(*handler, &source).is_some_and(|(_, f)| f == "HandlerFunc")
                {
                    arguments(*handler).first().copied().unwrap_or(*handler)
                } else {
                    *handler
                };
                let (target, symbol) =
                    resolve_handler(handler, &file, &source, input, &bindings, &mut functions);
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
                        label,
                        evidence: evidence(&file, node, &source),
                    },
                ));
            });
        }
        Ok(())
    }
}
