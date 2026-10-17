//! FastAPI: routes declared with `@app.get("/path")` / `@router.post(…)` and `Depends(provider)`.
use std::collections::HashMap;

use harness_workspace::{
    framework_relations::{
        declares, Certainty, FrameworkAdapter, FrameworkCapability, FrameworkInput,
        FrameworkOutput, FrameworkRelation, RelationDraft, RelationKind,
    },
    repository_intelligence::{DiscoveryError, Package},
};
use tokio_util::sync::CancellationToken;

use crate::{
    py_common::{local_names, python_files, resolve_view},
    py_items::{assignments, functions, join_path, string_value},
    syntax::{evidence, parse_python, text, walk},
};

const ID: &str = "fastapi";
const VERSION: u32 = 1;
const ROUTE_RULE: &str = "fastapi-route-decorator";
const DEPENDS_RULE: &str = "fastapi-depends";
const METHODS: [&str; 8] = [
    "get", "post", "put", "delete", "patch", "options", "head", "trace",
];

pub struct FastApiAdapter;

impl FrameworkAdapter for FastApiAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "FastAPI".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::RouteRegisteredTo, RelationKind::ProviderInjectedInto],
            handled: vec![
                "@app.get/post/put/delete/patch/options/head/trace(\"/path\") and @app.api_route(\"/path\", methods=[…]) on a variable created by FastAPI() or APIRouter() in the same file; an APIRouter(prefix=\"/x\") prefix is joined".into(),
                "parameters defaulting to Depends(provider) where the provider is a function of the repository".into(),
            ],
            unsupported: vec![
                "app.include_router(router, prefix=…) prefixes and routers imported from other files as the decorator receiver".into(),
                "app.add_api_route and computed paths (reported, no relation)".into(),
                "Depends inside Annotated[…] metadata and class-based dependencies".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        declares(package, "fastapi")
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
            if !source.contains("fastapi") && !source.contains("Depends") {
                continue;
            }
            let Some(tree) = parse_python(&source) else {
                continue;
            };
            let root = tree.root_node();
            let locals = local_names(root, &source);
            // receiver -> prefix, for the variables that are provably a FastAPI app or router.
            let mut receivers: HashMap<String, String> = HashMap::new();
            for (name, (callee, call)) in assignments(root, &source) {
                if callee == "FastAPI" {
                    receivers.insert(name, String::new());
                } else if callee == "APIRouter" {
                    let (_, kwargs) = crate::py_items::call_arguments(call, &source);
                    let prefix = kwargs
                        .iter()
                        .find(|(k, _)| k == "prefix")
                        .and_then(|(_, v)| string_value(*v, &source))
                        .unwrap_or_default();
                    receivers.insert(name, prefix);
                }
            }
            for function in functions(root, &source) {
                for decorator in &function.decorators {
                    let Some(receiver) = decorator.receiver.as_deref() else {
                        continue;
                    };
                    let is_route =
                        METHODS.contains(&decorator.name.as_str()) || decorator.name == "api_route";
                    if !is_route {
                        continue;
                    }
                    let path_node = decorator
                        .args
                        .first()
                        .copied()
                        .or_else(|| decorator.kwarg("path"));
                    let Some(prefix) = receivers.get(receiver) else {
                        if path_node
                            .and_then(|n| string_value(n, &source))
                            .is_some_and(|p| p.starts_with('/'))
                            && !receiver.contains('.')
                        {
                            output.unresolved(&file, format!("`@{receiver}.{}` looks like a route, but `{receiver}` is not provably a FastAPI app or router; no relation emitted", decorator.name));
                        }
                        continue;
                    };
                    let Some(path) = path_node.and_then(|n| string_value(n, &source)) else {
                        output.unresolved(
                            &file,
                            format!(
                                "`@{receiver}.{}` on `{}` has a computed path; no relation emitted",
                                decorator.name, function.name
                            ),
                        );
                        continue;
                    };
                    let methods: Vec<String> = if decorator.name == "api_route" {
                        let listed: Vec<String> = decorator
                            .kwarg("methods")
                            .map(|list| {
                                let mut cursor = list.walk();
                                list.named_children(&mut cursor)
                                    .filter_map(|m| string_value(m, &source))
                                    .map(|m| m.to_uppercase())
                                    .collect()
                            })
                            .unwrap_or_default();
                        if listed.is_empty() {
                            vec!["GET".into()]
                        } else {
                            listed
                        }
                    } else {
                        vec![decorator.name.to_uppercase()]
                    };
                    for method in methods {
                        output.relate(FrameworkRelation::new(
                            ID,
                            Some(&package.id),
                            RelationDraft {
                                kind: RelationKind::RouteRegisteredTo,
                                rule: ROUTE_RULE,
                                rule_version: VERSION,
                                certainty: Certainty::Declared,
                                from: file.clone(),
                                to: Some(file.clone()),
                                from_symbol: None,
                                to_symbol: Some(function.name.clone()),
                                label: format!("{method} {}", join_path(prefix, &path)),
                                evidence: evidence(&file, decorator.node, &source),
                            },
                        ));
                    }
                    if let Some(parameters) = function.parameters {
                        walk(parameters, &mut |node| {
                            if node.kind() != "call" {
                                return;
                            }
                            let Some(callee) = node.child_by_field_name("function") else {
                                return;
                            };
                            if text(callee, &source).rsplit('.').next() != Some("Depends") {
                                return;
                            }
                            let Some(provider) = node
                                .child_by_field_name("arguments")
                                .and_then(|a| a.named_child(0))
                                .filter(|a| matches!(a.kind(), "identifier" | "attribute"))
                            else {
                                return;
                            };
                            let (Some(provider_file), Some(name)) =
                                resolve_view(provider, &file, &source, &locals, input)
                            else {
                                return;
                            };
                            let name = name.as_str();
                            output.relate(FrameworkRelation::new(
                                ID,
                                Some(&package.id),
                                RelationDraft {
                                    kind: RelationKind::ProviderInjectedInto,
                                    rule: DEPENDS_RULE,
                                    rule_version: VERSION,
                                    certainty: Certainty::Declared,
                                    from: provider_file,
                                    to: Some(file.clone()),
                                    from_symbol: Some(name.to_owned()),
                                    to_symbol: Some(function.name.clone()),
                                    label: name.to_owned(),
                                    evidence: evidence(&file, node, &source),
                                },
                            ));
                        });
                    }
                }
            }
        }
        Ok(())
    }
}
