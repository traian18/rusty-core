//! Actix Web: routes written as attribute macros (`#[get("/path")]`) and as
//! `.route("/path", web::get().to(handler))`.
use harness_workspace::{
    framework_relations::{
        declares, Certainty, FrameworkAdapter, FrameworkCapability, FrameworkInput,
        FrameworkOutput, FrameworkRelation, RelationDraft, RelationKind,
    },
    repository_intelligence::{DiscoveryError, Package},
};
use tokio_util::sync::CancellationToken;

use crate::{
    rust_items::{first_string, functions, local_functions, resolve_path},
    syntax::{evidence, method_call_evidence, parse_rust, string_value, text, walk},
};

const ID: &str = "actix";
const VERSION: u32 = 1;
const ATTRIBUTE_RULE: &str = "actix-route-attribute";
const REGISTRATION_RULE: &str = "actix-route-registration";
const METHODS: [&str; 9] = [
    "get", "post", "put", "delete", "patch", "head", "options", "trace", "connect",
];

pub struct ActixAdapter;

impl FrameworkAdapter for ActixAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "Actix Web".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::RouteRegisteredTo],
            handled: vec![
                "#[get(\"/path\")], #[post(…)] and the other method attributes on a function, in files that use actix_web".into(),
                "#[route(\"/path\", method = \"GET\")]".into(),
                ".route(\"/path\", web::get().to(handler)) with a function, imported function, module::function or closure handler".into(),
            ],
            unsupported: vec![
                "web::scope and web::resource groupings, services registered through configure functions (reported once per file, no relation)".into(),
                "computed paths".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        declares(package, "actix-web")
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
            if !source.contains("actix_web") {
                continue;
            }
            let Some(tree) = parse_rust(&source) else {
                continue;
            };
            let root = tree.root_node();
            let file = &indexed.path;
            let locals = local_functions(root, &source);

            for function in functions(root, &source) {
                for attribute in &function.attributes {
                    let paths: Vec<(String, Option<String>)> =
                        if METHODS.contains(&attribute.path.as_str()) {
                            let path = attribute.arguments.and_then(|t| first_string(t, &source));
                            vec![(attribute.path.to_uppercase(), path)]
                        } else if attribute.path == "route" {
                            let tokens = attribute.arguments;
                            let path = tokens.and_then(|t| first_string(t, &source));
                            let mut methods = Vec::new();
                            if let Some(tokens) = tokens {
                                let mut cursor = tokens.walk();
                                let strings: Vec<_> = tokens
                                    .named_children(&mut cursor)
                                    .filter(|n| n.kind() == "string_literal")
                                    .collect();
                                for literal in strings.into_iter().skip(1) {
                                    if let Some(method) = string_value(literal, &source) {
                                        methods.push(method.to_uppercase());
                                    }
                                }
                            }
                            if methods.is_empty() {
                                methods.push("ANY".into());
                            }
                            methods.into_iter().map(|m| (m, path.clone())).collect()
                        } else {
                            continue;
                        };
                    for (method, path) in paths {
                        let Some(path) = path else {
                            output.unresolved(
                                file,
                                format!(
                                    "`#[{}]` on `{}` has no literal path; no relation emitted",
                                    attribute.path, function.name
                                ),
                            );
                            continue;
                        };
                        output.relate(FrameworkRelation::new(
                            ID,
                            Some(&package.id),
                            RelationDraft {
                                kind: RelationKind::RouteRegisteredTo,
                                rule: ATTRIBUTE_RULE,
                                rule_version: VERSION,
                                certainty: Certainty::Declared,
                                from: file.clone(),
                                to: Some(file.clone()),
                                from_symbol: None,
                                to_symbol: Some(function.name.clone()),
                                label: format!("{method} {path}"),
                                evidence: evidence(file, attribute.node, &source),
                            },
                        ));
                    }
                }
            }

            let mut grouped = false;
            walk(root, &mut |node| {
                if node.kind() != "call_expression" {
                    return;
                }
                let Some(function) = node.child_by_field_name("function") else {
                    return;
                };
                let callee = text(function, &source).replace(' ', "");
                if callee.ends_with("web::scope") || callee.ends_with("web::resource") {
                    grouped = true;
                    return;
                }
                if function.kind() != "field_expression"
                    || function
                        .child_by_field_name("field")
                        .map(|f| text(f, &source))
                        != Some("route")
                {
                    return;
                }
                let Some(arguments) = node.child_by_field_name("arguments") else {
                    return;
                };
                let mut cursor = arguments.walk();
                let args: Vec<_> = arguments.named_children(&mut cursor).collect();
                let [path_node, router, ..] = args.as_slice() else {
                    return;
                };
                let Some(path) = string_value(*path_node, &source) else {
                    output.unresolved(
                        file,
                        format!(
                            "`.route({}, …)` has a computed path; no relation emitted",
                            text(*path_node, &source)
                        ),
                    );
                    return;
                };
                // web::get().to(handler)
                let Some(chain) = router
                    .child_by_field_name("function")
                    .filter(|f| f.kind() == "field_expression")
                else {
                    return;
                };
                if chain.child_by_field_name("field").map(|f| text(f, &source)) != Some("to") {
                    return;
                }
                let method = chain
                    .child_by_field_name("value")
                    .and_then(|v| v.child_by_field_name("function"))
                    .map(|f| text(f, &source).replace(' ', ""))
                    .and_then(|f| f.strip_prefix("web::").map(str::to_owned));
                let Some(method) = method.filter(|m| METHODS.contains(&m.as_str())) else {
                    return;
                };
                let Some(handler) = router
                    .child_by_field_name("arguments")
                    .and_then(|a| a.named_child(0))
                else {
                    return;
                };
                let (target, symbol) = resolve_path(handler, file, &source, &locals, input);
                output.relate(FrameworkRelation::new(
                    ID,
                    Some(&package.id),
                    RelationDraft {
                        kind: RelationKind::RouteRegisteredTo,
                        rule: REGISTRATION_RULE,
                        rule_version: VERSION,
                        certainty: Certainty::Declared,
                        from: file.clone(),
                        to: target,
                        from_symbol: None,
                        to_symbol: symbol,
                        label: format!("{} {path}", method.to_uppercase()),
                        evidence: method_call_evidence(file, node, &source),
                    },
                ));
            });
            if grouped {
                output.unresolved(file, "this file registers routes through web::scope or web::resource; those groupings were not read");
            }
        }
        Ok(())
    }
}
