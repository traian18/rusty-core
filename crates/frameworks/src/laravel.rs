//! Laravel: routes in `routes/*.php`, Eloquent models and their tables, Artisan commands, and
//! container bindings matched to constructor parameters.
use std::collections::HashMap;

use rusty_repo_model::{
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
        let bindings = container_bindings(&parsed);
        for (file, source, tree) in &parsed {
            if is_routes_file(file) {
                let resolver = |name: &str| unique_class(&index, name);
                routes(
                    RouteRequest {
                        package,
                        file,
                        root: tree.root_node(),
                        source,
                        input,
                        unique: &resolver,
                    },
                    output,
                );
            }
            let context = FileRelationContext {
                package,
                file,
                source,
                in_controllers: is_controller_file(file),
                bindings: &bindings,
                index: &index,
            };
            emit_file_relations(&context, tree.root_node(), output);
        }
        Ok(())
    }
}

fn container_bindings(
    parsed: &[(RootRelativePath, String, tree_sitter::Tree)],
) -> HashMap<String, String> {
    let mut bindings = HashMap::new();
    for (_, source, tree) in parsed {
        walk(tree.root_node(), &mut |node| {
            collect_container_binding(node, source, &mut bindings);
        });
    }
    bindings
}

fn collect_container_binding(node: Node<'_>, source: &str, bindings: &mut HashMap<String, String>) {
    if node.kind() != "member_call_expression" || !is_container_binding_call(node, source) {
        return;
    }
    let args = arguments(node, "arguments", source);
    let (Some((_, interface)), Some((_, concrete))) = (args.first(), args.get(1)) else {
        return;
    };
    let (Some(interface), Some(concrete)) = (
        class_reference(*interface, source),
        class_reference(*concrete, source),
    ) else {
        return;
    };
    bindings.insert(interface, concrete);
}

fn is_container_binding_call(node: Node<'_>, source: &str) -> bool {
    let method = node
        .child_by_field_name("name")
        .map(|name| text(name, source))
        .unwrap_or("");
    if !matches!(method, "bind" | "singleton" | "scoped") {
        return false;
    }
    let object = node
        .child_by_field_name("object")
        .map(|object| text(object, source).replace(' ', ""))
        .unwrap_or_default();
    object.ends_with("app") || object.ends_with("$this->app")
}

fn unique_class(
    index: &HashMap<String, Vec<RootRelativePath>>,
    name: &str,
) -> Option<RootRelativePath> {
    let files = index.get(name)?;
    (files.len() == 1).then(|| files[0].clone())
}

fn is_routes_file(file: &RootRelativePath) -> bool {
    file.as_path()
        .components()
        .any(|component| component.as_os_str() == "routes")
}

fn is_controller_file(file: &RootRelativePath) -> bool {
    file.as_path()
        .to_string_lossy()
        .contains("Http/Controllers")
}

struct FileRelationContext<'a> {
    package: &'a Package,
    file: &'a RootRelativePath,
    source: &'a str,
    in_controllers: bool,
    bindings: &'a HashMap<String, String>,
    index: &'a HashMap<String, Vec<RootRelativePath>>,
}

fn emit_file_relations(
    context: &FileRelationContext<'_>,
    root: Node<'_>,
    output: &mut FrameworkOutput,
) {
    for class in classes(root, context.source) {
        emit_model_relation(
            context.package,
            context.file,
            context.source,
            &class,
            output,
        );
        emit_command_relation(
            context.package,
            context.file,
            context.source,
            &class,
            output,
        );
        emit_constructor_injections(context, &class, output);
    }
}

fn emit_model_relation(
    package: &Package,
    file: &RootRelativePath,
    source: &str,
    class: &crate::php_common::Class<'_>,
    output: &mut FrameworkOutput,
) {
    let base = class.base.as_deref().unwrap_or("");
    if !matches!(base, "Model" | "Authenticatable" | "Pivot") {
        return;
    }
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

fn emit_command_relation(
    package: &Package,
    file: &RootRelativePath,
    source: &str,
    class: &crate::php_common::Class<'_>,
    output: &mut FrameworkOutput,
) {
    if class.base.as_deref() != Some("Command") {
        return;
    }
    let Some(name) = class
        .properties
        .get("signature")
        .and_then(|signature| signature.split_whitespace().next())
        .filter(|name| !name.is_empty())
    else {
        return;
    };
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
            label: name.to_owned(),
            evidence: evidence(
                file,
                class.node.child_by_field_name("name").unwrap_or(class.node),
                source,
            ),
        },
    ));
}

fn emit_constructor_injections(
    context: &FileRelationContext<'_>,
    class: &crate::php_common::Class<'_>,
    output: &mut FrameworkOutput,
) {
    let Some(constructor) = class
        .methods
        .iter()
        .find(|method| method.name == "__construct")
    else {
        return;
    };
    for parameter in &constructor.parameters {
        let (provider_name, rule, certainty) = match context.bindings.get(&parameter.type_name) {
            Some(concrete) => (concrete.clone(), BINDING_RULE, Certainty::Declared),
            None if context.in_controllers => (
                parameter.type_name.clone(),
                AUTOWIRE_RULE,
                Certainty::Convention,
            ),
            None => continue,
        };
        let Some(provider) = unique_class(context.index, &provider_name) else {
            continue;
        };
        if &provider == context.file {
            continue;
        }
        output.relate(FrameworkRelation::new(
            ID,
            Some(&context.package.id),
            RelationDraft {
                kind: RelationKind::ProviderInjectedInto,
                rule,
                rule_version: VERSION,
                certainty,
                from: provider,
                to: Some(context.file.clone()),
                from_symbol: Some(provider_name.clone()),
                to_symbol: Some(class.name.clone()),
                label: provider_name,
                evidence: evidence(context.file, parameter.node, context.source),
            },
        ));
    }
}

fn chain_prefix(call: Node<'_>, source: &str) -> String {
    let mut prefix = String::new();
    let mut current = Some(call);
    while let Some(node) = current {
        collect_prefix(node, source, &mut prefix);
        current = chained_object(node);
    }
    prefix
}

fn collect_prefix(node: Node<'_>, source: &str, prefix: &mut String) {
    if !is_prefix_call(node, source) {
        return;
    }
    let arguments = arguments(node, "arguments", source);
    let Some((_, value)) = arguments.first() else {
        return;
    };
    let Some(part) = string_value(*value, source) else {
        return;
    };
    *prefix = format!("{}/{}", part.trim_matches('/'), prefix);
}

fn is_prefix_call(node: Node<'_>, source: &str) -> bool {
    node.child_by_field_name("name")
        .map(|name| text(name, source))
        == Some("prefix")
}

fn chained_object(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("object").filter(|object| {
        matches!(
            object.kind(),
            "member_call_expression" | "scoped_call_expression"
        )
    })
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

struct RouteRequest<'a, 'b> {
    package: &'a Package,
    file: &'a RootRelativePath,
    root: Node<'b>,
    source: &'a str,
    input: &'a FrameworkInput<'a>,
    unique: &'a dyn Fn(&str) -> Option<RootRelativePath>,
}

fn routes(request: RouteRequest<'_, '_>, output: &mut FrameworkOutput) {
    let bindings = import_bindings(request.input, request.file);
    let context = RouteContext {
        package: request.package,
        file: request.file,
        source: request.source,
        bindings: &bindings,
        unique: request.unique,
    };
    walk(request.root, &mut |node| {
        emit_route_call(&context, node, output)
    });
}

struct RouteContext<'a> {
    package: &'a Package,
    file: &'a RootRelativePath,
    source: &'a str,
    bindings: &'a HashMap<String, RootRelativePath>,
    unique: &'a dyn Fn(&str) -> Option<RootRelativePath>,
}

fn emit_route_call(context: &RouteContext<'_>, node: Node<'_>, output: &mut FrameworkOutput) {
    if node.kind() != "scoped_call_expression" || !is_route_scope(node, context.source) {
        return;
    }
    let method = call_name(node, context.source);
    let args = arguments(node, "arguments", context.source);
    let Some((verbs, path_index, handler_index)) = route_shape(&method, &args, context.source)
    else {
        return;
    };
    let Some((_, path_node)) = args.get(path_index) else {
        return;
    };
    let Some(path) = string_value(*path_node, context.source) else {
        context_error(context, &method, *path_node, output);
        return;
    };
    let (target, symbol) = args
        .get(handler_index)
        .map(|(_, handler)| route_handler(*handler, context))
        .unwrap_or((None, None));
    let route = RouteRecord {
        node,
        verbs,
        path,
        target,
        symbol,
    };
    emit_route_relations(context, &route, output);
}

fn context_error(
    context: &RouteContext<'_>,
    method: &str,
    path_node: Node<'_>,
    output: &mut FrameworkOutput,
) {
    output.unresolved(
        context.file,
        format!(
            "`Route::{method}({}, …)` has a computed path; no relation emitted",
            text(path_node, context.source)
        ),
    );
}

struct RouteRecord<'a> {
    node: Node<'a>,
    verbs: Vec<String>,
    path: String,
    target: Option<RootRelativePath>,
    symbol: Option<String>,
}

fn is_route_scope(node: Node<'_>, source: &str) -> bool {
    node.child_by_field_name("scope")
        .map(|scope| text(scope, source))
        == Some("Route")
}

fn call_name(node: Node<'_>, source: &str) -> String {
    node.child_by_field_name("name")
        .map(|name| text(name, source).to_owned())
        .unwrap_or_default()
}

fn route_shape(
    method: &str,
    args: &[(Option<String>, Node<'_>)],
    source: &str,
) -> Option<(Vec<String>, usize, usize)> {
    if VERBS.contains(&method) {
        return Some((vec![method.to_uppercase()], 0, 1));
    }
    if method == "match" {
        return Some((match_verbs(args, source), 1, 2));
    }
    matches!(method, "resource" | "apiResource").then(|| (vec!["RESOURCE".into()], 0, 1))
}

fn match_verbs(args: &[(Option<String>, Node<'_>)], source: &str) -> Vec<String> {
    let Some((_, values)) = args.first() else {
        return Vec::new();
    };
    let mut cursor = values.walk();
    values
        .named_children(&mut cursor)
        .filter_map(|element| element.named_child(0))
        .filter_map(|value| string_value(value, source))
        .map(|verb| verb.to_uppercase())
        .collect()
}

fn route_handler(
    handler: Node<'_>,
    context: &RouteContext<'_>,
) -> (Option<RootRelativePath>, Option<String>) {
    match handler.kind() {
        "array_creation_expression" => array_route_handler(handler, context),
        "string" => string_route_handler(handler, context),
        "anonymous_function" | "arrow_function" => (Some(context.file.clone()), None),
        _ => class_route_handler(handler, context),
    }
}

fn array_route_handler(
    handler: Node<'_>,
    context: &RouteContext<'_>,
) -> (Option<RootRelativePath>, Option<String>) {
    let mut cursor = handler.walk();
    let elements: Vec<_> = handler
        .named_children(&mut cursor)
        .filter_map(|element| element.named_child(0))
        .collect();
    let Some(class_node) = elements.first() else {
        return (None, None);
    };
    let Some(class) = class_reference(*class_node, context.source) else {
        return (None, None);
    };
    let action = elements
        .get(1)
        .and_then(|action| string_value(*action, context.source));
    let target = resolve_route_class(&class, context);
    let symbol = Some(action.map_or(class.clone(), |action| format!("{class}.{action}")));
    (target, symbol)
}

fn string_route_handler(
    handler: Node<'_>,
    context: &RouteContext<'_>,
) -> (Option<RootRelativePath>, Option<String>) {
    let value = string_value(handler, context.source).unwrap_or_default();
    let (class, action) = value
        .split_once('@')
        .map_or((value.as_str(), None), |(class, action)| {
            (class, Some(action))
        });
    let class = class.rsplit('\\').next().unwrap_or(class);
    let target = resolve_route_class(class, context);
    let symbol = Some(action.map_or(class.to_owned(), |action| format!("{class}.{action}")));
    (target, symbol)
}

fn class_route_handler(
    handler: Node<'_>,
    context: &RouteContext<'_>,
) -> (Option<RootRelativePath>, Option<String>) {
    let class = class_reference(handler, context.source);
    let target = class
        .as_deref()
        .and_then(|name| resolve_route_class(name, context));
    (target, class)
}

fn resolve_route_class(name: &str, context: &RouteContext<'_>) -> Option<RootRelativePath> {
    context
        .bindings
        .get(name)
        .cloned()
        .or_else(|| (context.unique)(name))
}

fn emit_route_relations(
    context: &RouteContext<'_>,
    route: &RouteRecord<'_>,
    output: &mut FrameworkOutput,
) {
    let prefix = group_prefix(route.node, context.source);
    for verb in &route.verbs {
        output.relate(FrameworkRelation::new(
            ID,
            Some(&context.package.id),
            RelationDraft {
                kind: RelationKind::RouteRegisteredTo,
                rule: ROUTE_RULE,
                rule_version: VERSION,
                certainty: Certainty::Declared,
                from: context.file.clone(),
                to: route.target.clone(),
                from_symbol: None,
                to_symbol: route.symbol.clone(),
                label: format!("{verb} {}", join(&prefix, &route.path)),
                evidence: evidence(context.file, route.node, context.source),
            },
        ));
    }
}

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
