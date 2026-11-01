//! Go's standard library: `http.HandleFunc("/path", handler)` and the same calls on a
//! `http.NewServeMux()` variable. Go 1.22 patterns ("GET /path") keep their method.
use std::collections::HashSet;

use rusty_repo_model::{
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
            let mut scan = FileScan {
                package,
                input,
                file: &file,
                source: &source,
                muxes: muxes_of(root, &source),
                bindings: import_bindings(input, &file),
                functions: &mut functions,
            };
            walk(root, &mut |node| scan.registration(node, output));
        }
        Ok(())
    }
}

/// The names that route through `net/http`: the package itself and every `http.NewServeMux()`.
fn muxes_of(root: tree_sitter::Node<'_>, source: &str) -> HashSet<String> {
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
            && selector_call(call, source).is_some_and(|(operand, field)| {
                text(operand, source) == "http" && field == "NewServeMux"
            })
        {
            muxes.insert(text(name, source).to_owned());
        }
    });
    muxes
}

/// A `HandleFunc`-style pattern as a label: `GET /path`, or `ANY pattern` without a verb.
fn route_label(pattern: &str) -> String {
    match pattern.split_once(' ') {
        Some((verb, path)) if verb.chars().all(|c| c.is_ascii_uppercase()) => {
            format!("{verb} {}", path.trim())
        }
        _ => format!("ANY {pattern}"),
    }
}

/// One Go file being read for `net/http` routes.
struct FileScan<'a> {
    package: &'a Package,
    input: &'a FrameworkInput<'a>,
    file: &'a rusty_repo_model::repository_intelligence::RootRelativePath,
    source: &'a str,
    muxes: HashSet<String>,
    bindings: std::collections::HashMap<
        String,
        rusty_repo_model::repository_intelligence::RootRelativePath,
    >,
    functions: &'a mut PackageFunctions,
}

impl FileScan<'_> {
    /// Relates `mux.Handle(pattern, handler)` and its siblings to the handler.
    fn registration(&mut self, node: tree_sitter::Node<'_>, output: &mut FrameworkOutput) {
        let source = self.source;
        if node.kind() != "call_expression" {
            return;
        }
        let Some((operand, method)) = selector_call(node, source) else {
            return;
        };
        if !METHODS.contains(&method.as_str()) || !self.muxes.contains(text(operand, source)) {
            return;
        }
        let args = arguments(node);
        let [pattern_node, handler, ..] = args.as_slice() else {
            return;
        };
        let Some(pattern) = string_value(*pattern_node, source) else {
            output.unresolved(
                self.file,
                format!(
                    "`{}.{method}({}, …)` has a computed pattern; no relation emitted",
                    text(operand, source),
                    text(*pattern_node, source)
                ),
            );
            return;
        };
        // `http.HandlerFunc(fn)` wraps a function; follow the function.
        let handler = if handler.kind() == "call_expression"
            && selector_call(*handler, source).is_some_and(|(_, f)| f == "HandlerFunc")
        {
            arguments(*handler).first().copied().unwrap_or(*handler)
        } else {
            *handler
        };
        let (target, symbol) = resolve_handler(
            handler,
            self.file,
            source,
            self.input,
            &self.bindings,
            self.functions,
        );
        let draft = RelationDraft {
            kind: RelationKind::RouteRegisteredTo,
            rule: RULE,
            rule_version: VERSION,
            certainty: Certainty::Declared,
            from: self.file.clone(),
            to: target,
            from_symbol: None,
            to_symbol: symbol,
            label: route_label(&pattern),
            evidence: evidence(self.file, node, source),
        };
        output.relate(FrameworkRelation::new(ID, Some(&self.package.id), draft));
    }
}
