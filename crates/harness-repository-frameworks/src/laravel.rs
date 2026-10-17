//! Laravel: routes in `routes/*.php`, Eloquent models and their tables, Artisan commands, and
//! container bindings matched to constructor parameters.
use std::collections::HashMap;

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
    php_common::{
        arguments, class_index, class_reference, classes, php_files, plural, snake_case,
        string_value,
    },
    syntax::{evidence, text, walk},
};

const ID: &str = "laravel";
const VERSION: u32 = 1;
const ROUTE_RULE: &str = "laravel-route-facade";
const MODEL_RULE: &str = "laravel-eloquent-table";
const COMMAND_RULE: &str = "laravel-artisan-signature";
const BINDING_RULE: &str = "laravel-container-binding";
const AUTOWIRE_RULE: &str = "laravel-controller-autowiring";
const VERBS: [&str; 7] = ["get", "post", "put", "patch", "delete", "options", "any"];

pub struct LaravelAdapter;

impl FrameworkAdapter for LaravelAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "Laravel".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::RouteRegisteredTo, RelationKind::ProviderInjectedInto, RelationKind::ModelMappedToTable, RelationKind::CommandExposedAs],
            handled: vec![
                "Route::get/post/put/patch/delete/options/any and Route::match with a literal path in files under routes/, handlers as [Controller::class, 'method'], 'Controller@method', or a closure; Route::resource and Route::apiResource (one relation); ->prefix('x')->group(function () { … }) prefixes are joined".into(),
                "Eloquent models (classes deriving from Model): $table when literal (declared), otherwise the pluralised snake-case class name (convention)".into(),
                "Artisan commands: classes deriving from Command with a literal $signature".into(),
                "$this->app->bind/singleton/scoped(Interface::class, Concrete::class) matched to constructor parameters; constructor parameters of controllers that name a class of the repository (autowired, a convention)".into(),
            ],
            unsupported: vec![
                "the implicit /api prefix of routes/api.php, Route::group([...], fn) arrays, route model binding and middleware".into(),
                "Route::view/redirect, Route::controller groups and routes built from variables (reported when the path is computed)".into(),
                "bindings in service provider arrays ($bindings, $singletons) and contextual bindings".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        declares(package, "laravel/framework")
    }

    fn extract(
        &self,
        package: &Package,
        input: &FrameworkInput<'_>,
        cancel: &CancellationToken,
        output: &mut FrameworkOutput,
    ) -> Result<(), DiscoveryError> {
        let mut parsed = Vec::new();
        for file in php_files(package, input) {
            if cancel.is_cancelled() {
                return Err(DiscoveryError::Cancelled);
            }
            let Some(source) = input.read(&file) else {
                continue;
            };
            let Some(tree) = crate::syntax::parse_php(&source) else {
                continue;
            };
            parsed.push((file, source, tree));
        }
        let index = class_index(&parsed);
        let unique = |name: &str| {
            index
                .get(name)
                .filter(|f| f.len() == 1)
                .map(|f| f[0].clone())
        };

        // Container bindings: interface -> implementation.
        let mut bindings: HashMap<String, String> = HashMap::new();
        for (_, source, tree) in &parsed {
            walk(tree.root_node(), &mut |node| {
                if node.kind() != "member_call_expression" {
                    return;
                }
                let method = node
                    .child_by_field_name("name")
                    .map(|n| text(n, source))
                    .unwrap_or("");
                if !matches!(method, "bind" | "singleton" | "scoped") {
                    return;
                }
                let object = node
                    .child_by_field_name("object")
                    .map(|o| text(o, source).replace(' ', ""))
                    .unwrap_or_default();
                if !object.ends_with("app") && !object.ends_with("$this->app") {
                    return;
                }
                let args = arguments(node, "arguments", source);
                if let (Some((_, interface)), Some((_, concrete))) = (args.first(), args.get(1)) {
                    if let (Some(i), Some(c)) = (
                        class_reference(*interface, source),
                        class_reference(*concrete, source),
                    ) {
                        bindings.insert(i, c);
                    }
                }
            });
        }

        for (file, source, tree) in &parsed {
            let root = tree.root_node();
            let in_routes = file
                .as_path()
                .components()
                .any(|c| c.as_os_str() == "routes");
            if in_routes {
                routes(package, file, root, source, input, &unique, output);
            }
            let in_controllers = file
                .as_path()
                .to_string_lossy()
                .contains("Http/Controllers");
            for class in classes(root, source) {
                let base = class.base.as_deref().unwrap_or("");
                if matches!(base, "Model" | "Authenticatable" | "Pivot") {
                    let (label, certainty) = match class.properties.get("table") {
                        Some(table) => (table.clone(), Certainty::Declared),
                        None => (plural(&snake_case(&class.name)), Certainty::Convention),
                    };
                    output.relate(FrameworkRelation::new(
                        ID,
                        Some(&package.id),
                        RelationDraft {
                            kind: RelationKind::ModelMappedToTable,
                            rule: MODEL_RULE,
                            rule_version: VERSION,
                            certainty,
                            from: file.clone(),
                            to: None,
                            from_symbol: Some(class.name.clone()),
                            to_symbol: None,
                            label,
                            evidence: evidence(
                                file,
                                class.node.child_by_field_name("name").unwrap_or(class.node),
                                source,
                            ),
                        },
                    ));
                }
                if base == "Command" {
                    if let Some(signature) = class.properties.get("signature") {
                        let name = signature.split_whitespace().next().unwrap_or("").to_owned();
                        if !name.is_empty() {
                            output.relate(FrameworkRelation::new(
                                ID,
                                Some(&package.id),
                                RelationDraft {
                                    kind: RelationKind::CommandExposedAs,
                                    rule: COMMAND_RULE,
                                    rule_version: VERSION,
                                    certainty: Certainty::Declared,
                                    from: file.clone(),
                                    to: Some(file.clone()),
                                    from_symbol: None,
                                    to_symbol: Some(class.name.clone()),
                                    label: name,
                                    evidence: evidence(
                                        file,
                                        class
                                            .node
                                            .child_by_field_name("name")
                                            .unwrap_or(class.node),
                                        source,
                                    ),
                                },
                            ));
                        }
                    }
                }
                if let Some(constructor) = class.constructor() {
                    for parameter in &constructor.parameters {
                        let (provider_name, rule, certainty) = match bindings
                            .get(&parameter.type_name)
                        {
                            Some(concrete) => (concrete.clone(), BINDING_RULE, Certainty::Declared),
                            None if in_controllers => (
                                parameter.type_name.clone(),
                                AUTOWIRE_RULE,
                                Certainty::Convention,
                            ),
                            None => continue,
                        };
                        let Some(provider) = unique(&provider_name) else {
                            continue;
                        };
                        if &provider == file {
                            continue;
                        }
                        output.relate(FrameworkRelation::new(
                            ID,
                            Some(&package.id),
                            RelationDraft {
                                kind: RelationKind::ProviderInjectedInto,
                                rule,
                                rule_version: VERSION,
                                certainty,
                                from: provider,
                                to: Some(file.clone()),
                                from_symbol: Some(provider_name.clone()),
                                to_symbol: Some(class.name.clone()),
                                label: provider_name,
                                evidence: evidence(file, parameter.node, source),
                            },
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

/// `->prefix('api')` literals in the chain leading to a `->group(...)` call.
fn chain_prefix(call: Node<'_>, source: &str) -> String {
    let mut prefix = String::new();
    let mut current = Some(call);
    while let Some(node) = current {
        let name = node
            .child_by_field_name("name")
            .map(|n| text(n, source))
            .unwrap_or("");
        if name == "prefix" {
            if let Some((_, value)) = arguments(node, "arguments", source).first() {
                if let Some(part) = string_value(*value, source) {
                    prefix = format!("{}/{}", part.trim_matches('/'), prefix);
                }
            }
        }
        current = node.child_by_field_name("object").filter(|o| {
            matches!(
                o.kind(),
                "member_call_expression" | "scoped_call_expression"
            )
        });
    }
    prefix
}

fn group_prefix(node: Node<'_>, source: &str) -> String {
    let mut prefix = String::new();
    let mut current = node.parent();
    while let Some(ancestor) = current {
        if ancestor.kind() == "member_call_expression"
            && ancestor
                .child_by_field_name("name")
                .map(|n| text(n, source))
                == Some("group")
        {
            if let Some(object) = ancestor.child_by_field_name("object") {
                prefix = format!(
                    "{}/{}",
                    chain_prefix(object, source).trim_matches('/'),
                    prefix
                );
            }
        }
        current = ancestor.parent();
    }
    prefix
}

fn join(prefix: &str, path: &str) -> String {
    let parts: Vec<&str> = [prefix, path]
        .iter()
        .flat_map(|p| p.split('/'))
        .filter(|p| !p.is_empty())
        .collect();
    format!("/{}", parts.join("/"))
}

fn routes(
    package: &Package,
    file: &RootRelativePath,
    root: Node<'_>,
    source: &str,
    input: &FrameworkInput<'_>,
    unique: &dyn Fn(&str) -> Option<RootRelativePath>,
    output: &mut FrameworkOutput,
) {
    let bindings = import_bindings(input, file);
    let resolve = |name: &str| bindings.get(name).cloned().or_else(|| unique(name));
    walk(root, &mut |node| {
        if node.kind() != "scoped_call_expression" {
            return;
        }
        let scope = node
            .child_by_field_name("scope")
            .map(|s| text(s, source))
            .unwrap_or("");
        if scope != "Route" {
            return;
        }
        let method = node
            .child_by_field_name("name")
            .map(|n| text(n, source).to_owned())
            .unwrap_or_default();
        let args = arguments(node, "arguments", source);
        let prefix = group_prefix(node, source);
        let (verbs, path_at, handler_at): (Vec<String>, usize, usize) =
            if VERBS.contains(&method.as_str()) {
                (vec![method.to_uppercase()], 0, 1)
            } else if method == "match" {
                let verbs = args.first().map(|(_, v)| {
                    let mut cursor = v.walk();
                    v.named_children(&mut cursor)
                        .filter_map(|e| e.named_child(0))
                        .filter_map(|s| string_value(s, source))
                        .map(|s| s.to_uppercase())
                        .collect::<Vec<_>>()
                });
                (verbs.unwrap_or_default(), 1, 2)
            } else if matches!(method.as_str(), "resource" | "apiResource") {
                (vec!["RESOURCE".into()], 0, 1)
            } else {
                return;
            };
        let Some((_, path_node)) = args.get(path_at) else {
            return;
        };
        let Some(path) = string_value(*path_node, source) else {
            output.unresolved(
                file,
                format!(
                    "`Route::{method}({}, …)` has a computed path; no relation emitted",
                    text(*path_node, source)
                ),
            );
            return;
        };
        let (target, symbol) = match args.get(handler_at).map(|(_, v)| *v) {
            Some(handler) if handler.kind() == "array_creation_expression" => {
                let mut cursor = handler.walk();
                let elements: Vec<Node<'_>> = handler
                    .named_children(&mut cursor)
                    .filter_map(|e| e.named_child(0))
                    .collect();
                let class = elements.first().and_then(|c| class_reference(*c, source));
                let action = elements.get(1).and_then(|a| string_value(*a, source));
                (
                    class.as_deref().and_then(&resolve),
                    class.map(|c| action.map_or(c.clone(), |a| format!("{c}.{a}"))),
                )
            }
            Some(handler) if handler.kind() == "string" => {
                let value = string_value(handler, source).unwrap_or_default();
                let (class, action) = value
                    .split_once('@')
                    .map_or((value.as_str(), None), |(c, a)| (c, Some(a)));
                let class = class.rsplit('\\').next().unwrap_or(class);
                (
                    resolve(class),
                    Some(action.map_or(class.to_owned(), |a| format!("{class}.{a}"))),
                )
            }
            Some(handler) if matches!(handler.kind(), "anonymous_function" | "arrow_function") => {
                (Some(file.clone()), None)
            }
            Some(handler) => {
                let class = class_reference(handler, source);
                (class.as_deref().and_then(&resolve), class)
            }
            None => (None, None),
        };
        for verb in verbs {
            output.relate(FrameworkRelation::new(
                ID,
                Some(&package.id),
                RelationDraft {
                    kind: RelationKind::RouteRegisteredTo,
                    rule: ROUTE_RULE,
                    rule_version: VERSION,
                    certainty: Certainty::Declared,
                    from: file.clone(),
                    to: target.clone(),
                    from_symbol: None,
                    to_symbol: symbol.clone(),
                    label: format!("{verb} {}", join(&prefix, &path)),
                    evidence: evidence(file, node, source),
                },
            ));
        }
    });
}

/// Imported simple class name -> the file its `use` resolved to.
fn import_bindings(
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
        let binding = edge
            .fact
            .alias
            .clone()
            .or_else(|| edge.fact.imported_name.clone());
        if let Some(binding) = binding {
            found.entry(binding).or_insert_with(|| target.clone());
        }
    }
    found
}
