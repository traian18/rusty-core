//! Flask: routes declared with `@app.route("/path")`, the 2.x shortcuts, and `add_url_rule`.
use std::collections::HashMap;

use rusty_repo_model::{
    framework_relations::{
        declares, Certainty, FrameworkAdapter, FrameworkCapability, FrameworkInput,
        FrameworkOutput, FrameworkRelation, RelationDraft, RelationKind,
    },
    repository_intelligence::{DiscoveryError, Package},
};
use tokio_util::sync::CancellationToken;

use crate::{
    py_common::{local_names, python_files, resolve_view},
    py_items::{assignments, call_arguments, functions, join_path, string_value},
    syntax::{evidence, parse_python, text, walk},
};

const ID: &str = "flask";
const VERSION: u32 = 1;
const DECORATOR_RULE: &str = "flask-route-decorator";
const RULE_RULE: &str = "flask-add-url-rule";
const SHORTCUTS: [&str; 5] = ["get", "post", "put", "delete", "patch"];

pub struct FlaskAdapter;

impl FrameworkAdapter for FlaskAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "Flask".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::RouteRegisteredTo],
            handled: vec![
                "@app.route(\"/path\", methods=[…]) and @app.get/post/put/delete/patch(\"/path\") on a variable created by Flask() or Blueprint() in the same file; Blueprint url_prefix is joined".into(),
                "app.add_url_rule(\"/path\", view_func=function) on such a variable".into(),
            ],
            unsupported: vec![
                "app.register_blueprint(bp, url_prefix=…) prefixes and receivers created in other files".into(),
                "MethodView classes, flask-restful resources and computed paths (reported, no relation)".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        declares(package, "flask")
    }

    fn extract(
        &self,
        package: &Package,
        input: &FrameworkInput<'_>,
        cancel: &CancellationToken,
        output: &mut FrameworkOutput,
    ) -> Result<(), DiscoveryError> {
        for file in python_files(package, input) {
            if cancel.is_cancelled() {
                return Err(DiscoveryError::Cancelled);
            }
            let Some(source) = input.read(&file) else {
                continue;
            };
            if !source.contains("flask") && !source.contains("Flask") {
                continue;
            }
            let Some(tree) = parse_python(&source) else {
                continue;
            };
            let root = tree.root_node();
            let scan = FileScan {
                package,
                input,
                file: &file,
                source: &source,
                locals: local_names(root, &source),
                receivers: receivers_of(root, &source),
            };
            scan.decorated_routes(root, output);
            scan.add_url_rules(root, output);
        }
        Ok(())
    }
}

/// Variables created by `Flask()` (empty prefix) or `Blueprint()` (its `url_prefix`).
fn receivers_of(root: tree_sitter::Node<'_>, source: &str) -> HashMap<String, String> {
    let mut receivers = HashMap::new();
    for (name, (callee, call)) in assignments(root, source) {
        if callee == "Flask" {
            receivers.insert(name, String::new());
        } else if callee == "Blueprint" {
            let (_, kwargs) = call_arguments(call, source);
            let prefix = kwargs
                .iter()
                .find(|(k, _)| k == "url_prefix")
                .and_then(|(_, v)| string_value(*v, source))
                .unwrap_or_default();
            receivers.insert(name, prefix);
        }
    }
    receivers
}

/// The HTTP methods a `methods=[…]` list names; `GET` when there is none.
fn methods_of(node: Option<tree_sitter::Node<'_>>, source: &str) -> Vec<String> {
    let listed: Vec<String> = node
        .map(|list| {
            let mut cursor = list.walk();
            list.named_children(&mut cursor)
                .filter_map(|m| string_value(m, source))
                .map(|m| m.to_uppercase())
                .collect()
        })
        .unwrap_or_default();
    if listed.is_empty() {
        vec!["GET".into()]
    } else {
        listed
    }
}

/// One Python file being read for Flask routes.
struct FileScan<'a> {
    package: &'a Package,
    input: &'a FrameworkInput<'a>,
    file: &'a rusty_repo_model::repository_intelligence::RootRelativePath,
    source: &'a str,
    locals: std::collections::HashSet<String>,
    receivers: HashMap<String, String>,
}

impl FileScan<'_> {
    /// `@app.route(...)` and the `@app.get(...)` shortcuts on a known app or blueprint.
    fn decorated_routes(&self, root: tree_sitter::Node<'_>, output: &mut FrameworkOutput) {
        for function in functions(root, self.source) {
            for decorator in &function.decorators {
                let Some(receiver) = decorator.receiver.as_deref() else {
                    continue;
                };
                if decorator.name != "route" && !SHORTCUTS.contains(&decorator.name.as_str()) {
                    continue;
                }
                let first = decorator
                    .args
                    .first()
                    .and_then(|n| string_value(*n, self.source));
                let Some(prefix) = self.receivers.get(receiver) else {
                    if first.is_some_and(|p| p.starts_with('/')) && !receiver.contains('.') {
                        output.unresolved(self.file, format!("`@{receiver}.{}` looks like a route, but `{receiver}` is not provably a Flask app or blueprint; no relation emitted", decorator.name));
                    }
                    continue;
                };
                let Some(path) = first else {
                    output.unresolved(
                        self.file,
                        format!(
                            "`@{receiver}.{}` on `{}` has a computed path; no relation emitted",
                            decorator.name, function.name
                        ),
                    );
                    continue;
                };
                let methods = if decorator.name == "route" {
                    methods_of(decorator.kwarg("methods"), self.source)
                } else {
                    vec![decorator.name.to_uppercase()]
                };
                for method in methods {
                    let draft = RelationDraft {
                        kind: RelationKind::RouteRegisteredTo,
                        rule: DECORATOR_RULE,
                        rule_version: VERSION,
                        certainty: Certainty::Declared,
                        from: self.file.clone(),
                        to: Some(self.file.clone()),
                        from_symbol: None,
                        to_symbol: Some(function.name.clone()),
                        label: format!("{method} {}", join_path(prefix, &path)),
                        evidence: evidence(self.file, decorator.node, self.source),
                    };
                    output.relate(FrameworkRelation::new(ID, Some(&self.package.id), draft));
                }
            }
        }
    }

    /// `app.add_url_rule("/path", view_func=…)` on a known app or blueprint.
    fn add_url_rules(&self, root: tree_sitter::Node<'_>, output: &mut FrameworkOutput) {
        walk(root, &mut |node| {
            if node.kind() == "call" {
                self.add_url_rule(node, output);
            }
        });
    }

    fn add_url_rule(&self, node: tree_sitter::Node<'_>, output: &mut FrameworkOutput) {
        let source = self.source;
        let Some(function) = node
            .child_by_field_name("function")
            .filter(|f| f.kind() == "attribute")
        else {
            return;
        };
        let called = function
            .child_by_field_name("attribute")
            .map(|a| text(a, source));
        if called != Some("add_url_rule") {
            return;
        }
        let Some(receiver) = function
            .child_by_field_name("object")
            .map(|o| text(o, source))
        else {
            return;
        };
        let Some(prefix) = self.receivers.get(receiver) else {
            return;
        };
        let (args, kwargs) = call_arguments(node, source);
        let Some(path) = args.first().and_then(|n| string_value(*n, source)) else {
            output.unresolved(
                self.file,
                format!("`{receiver}.add_url_rule` has a computed path; no relation emitted"),
            );
            return;
        };
        let view = kwargs
            .iter()
            .find(|(k, _)| k == "view_func")
            .map(|(_, v)| *v)
            .or_else(|| args.get(2).copied());
        let (target, symbol) = view.map_or((None, None), |v| {
            resolve_view(v, self.file, source, &self.locals, self.input)
        });
        let listed = kwargs.iter().find(|(k, _)| k == "methods").map(|(_, v)| *v);
        for method in methods_of(listed, source) {
            let draft = RelationDraft {
                kind: RelationKind::RouteRegisteredTo,
                rule: RULE_RULE,
                rule_version: VERSION,
                certainty: Certainty::Declared,
                from: self.file.clone(),
                to: target.clone(),
                from_symbol: None,
                to_symbol: symbol.clone(),
                label: format!("{method} {}", join_path(prefix, &path)),
                evidence: evidence(self.file, node, source),
            };
            output.relate(FrameworkRelation::new(ID, Some(&self.package.id), draft));
        }
    }
}
