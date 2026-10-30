//! ASP.NET Core: controller routes from attributes, minimal API routes, dependency-injection
//! registrations matched to constructor parameters, and EF Core `DbSet` tables.
use std::collections::HashMap;

use harness_workspace::{
    framework_relations::{
        Certainty, FrameworkAdapter, FrameworkCapability, FrameworkInput, FrameworkOutput,
        FrameworkRelation, RelationDraft, RelationKind,
    },
    repository_intelligence::{DiscoveryError, Package, RootRelativePath},
};
use tokio_util::sync::CancellationToken;
use tree_sitter::Node;

use crate::syntax::{evidence, parse_csharp, text, walk};

const ID: &str = "aspnetcore";
const VERSION: u32 = 1;
const CONTROLLER_RULE: &str = "aspnet-controller-attribute";
const MINIMAL_RULE: &str = "aspnet-minimal-api";
const DI_RULE: &str = "aspnet-di-registration";
const EF_RULE: &str = "efcore-dbset";
const VERBS: [(&str, &str); 7] = [
    ("HttpGet", "GET"),
    ("HttpPost", "POST"),
    ("HttpPut", "PUT"),
    ("HttpDelete", "DELETE"),
    ("HttpPatch", "PATCH"),
    ("HttpHead", "HEAD"),
    ("HttpOptions", "OPTIONS"),
];
const MAPS: [(&str, &str); 5] = [
    ("MapGet", "GET"),
    ("MapPost", "POST"),
    ("MapPut", "PUT"),
    ("MapDelete", "DELETE"),
    ("MapPatch", "PATCH"),
];
const REGISTRATIONS: [&str; 6] = [
    "AddScoped",
    "AddSingleton",
    "AddTransient",
    "TryAddScoped",
    "TryAddSingleton",
    "TryAddTransient",
];

pub struct AspNetAdapter;

impl FrameworkAdapter for AspNetAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "ASP.NET Core".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::RouteRegisteredTo, RelationKind::ProviderInjectedInto, RelationKind::ModelMappedToTable],
            handled: vec![
                "controllers ([ApiController] or deriving from Controller/ControllerBase) with [Route], [HttpGet/Post/Put/Delete/Patch/Head/Options] and literal templates; [controller] and [action] tokens are expanded (a convention)".into(),
                "minimal APIs: app.MapGet/MapPost/MapPut/MapDelete/MapPatch with a literal pattern on the result of builder.Build(), and groups from MapGroup(\"/prefix\")".into(),
                "services.AddScoped/AddSingleton/AddTransient<TService, TImplementation>() (and <T>) matched to constructor parameters of classes in the repository".into(),
                "EF Core: DbSet<T> properties of a DbContext map T to [Table(\"…\")] when present (declared), else the property name (convention)".into(),
            ],
            unsupported: vec![
                "conventional routing (MapControllerRoute), areas, versioned routes and route constraints beyond the template text".into(),
                "registrations through extension methods, factories, keyed services or typeof(…) overloads".into(),
                "types with the same name in several namespaces (a registration or entity is matched only when the name is unique in the package)".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        package
            .facts
            .declared_dependencies
            .iter()
            .find(|d| {
                d.as_str() == "Microsoft.NET.Sdk.Web" || d.starts_with("Microsoft.AspNetCore")
            })
            .map(|d| format!("declares `{d}`"))
    }

    fn extract(
        &self,
        package: &Package,
        input: &FrameworkInput<'_>,
        cancel: &CancellationToken,
        output: &mut FrameworkOutput,
    ) -> Result<(), DiscoveryError> {
        let parsed = parse_package_files(package, input, cancel)?;
        let catalog = TypeCatalog::from_parsed(&parsed);
        let registrations = collect_registrations(&parsed);
        for parsed_file in &parsed {
            let classes = classes(parsed_file.tree.root_node(), &parsed_file.source);
            for class in &classes {
                emit_class_relations(
                    package,
                    parsed_file,
                    class,
                    &catalog,
                    &registrations,
                    output,
                );
            }
            minimal_apis(
                package,
                &parsed_file.file,
                parsed_file.tree.root_node(),
                &parsed_file.source,
                &|name| catalog.unique(name),
                output,
            );
        }
        Ok(())
    }
}

struct ParsedFile {
    file: RootRelativePath,
    source: String,
    tree: tree_sitter::Tree,
}

fn collect_registrations(parsed: &[ParsedFile]) -> HashMap<String, String> {
    let mut registrations = HashMap::new();
    for parsed_file in parsed {
        collect_file_registrations(parsed_file, &mut registrations);
    }
    registrations
}

fn collect_file_registrations(parsed: &ParsedFile, registrations: &mut HashMap<String, String>) {
    walk(parsed.tree.root_node(), &mut |node| {
        if let Some((service, implementation)) = registration(node, &parsed.source) {
            registrations.insert(service, implementation);
        }
    });
}

fn registration(node: Node<'_>, source: &str) -> Option<(String, String)> {
    if node.kind() != "invocation_expression" {
        return None;
    }
    let function = node
        .child_by_field_name("function")
        .filter(|function| function.kind() == "member_access_expression")?;
    let name = function
        .child_by_field_name("name")
        .filter(|name| name.kind() == "generic_name")?;
    let method = name.named_child(0).map(|node| text(node, source))?;
    if !REGISTRATIONS.contains(&method) {
        return None;
    }
    let arguments = registration_type_arguments(name, source);
    match arguments.as_slice() {
        [only] => Some((only.clone(), only.clone())),
        [service, implementation] => Some((service.clone(), implementation.clone())),
        _ => None,
    }
}

fn registration_type_arguments(name: Node<'_>, source: &str) -> Vec<String> {
    let Some(list) = name.named_child(1) else {
        return Vec::new();
    };
    let mut cursor = list.walk();
    list.named_children(&mut cursor)
        .filter(|ty| matches!(ty.kind(), "identifier" | "generic_name"))
        .map(|ty| simple_name(ty, source))
        .collect()
}

struct TypeCatalog {
    types: HashMap<String, Vec<RootRelativePath>>,
    tables: HashMap<String, String>,
}

impl TypeCatalog {
    fn from_parsed(parsed: &[ParsedFile]) -> Self {
        let mut catalog = Self {
            types: HashMap::new(),
            tables: HashMap::new(),
        };
        for parsed_file in parsed {
            for class in classes(parsed_file.tree.root_node(), &parsed_file.source) {
                catalog
                    .types
                    .entry(class.name.clone())
                    .or_default()
                    .push(parsed_file.file.clone());
                if let Some(table) = table_name(&class) {
                    catalog.tables.insert(class.name, table);
                }
            }
        }
        catalog
    }

    fn unique(&self, name: &str) -> Option<RootRelativePath> {
        let files = self.types.get(name)?;
        (files.len() == 1).then(|| files[0].clone())
    }
}

fn table_name(class: &Class<'_>) -> Option<String> {
    class
        .attributes
        .iter()
        .find(|attribute| attribute.name == "Table")
        .and_then(|attribute| attribute.first_string.clone())
}

fn parse_package_files(
    package: &Package,
    input: &FrameworkInput<'_>,
    cancel: &CancellationToken,
) -> Result<Vec<ParsedFile>, DiscoveryError> {
    let mut parsed = Vec::new();
    for indexed in input.files_of(package) {
        if cancel.is_cancelled() {
            return Err(DiscoveryError::Cancelled);
        }
        if indexed
            .path
            .as_path()
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("cs")
        {
            continue;
        }
        let Some(source) = input.read(&indexed.path) else {
            continue;
        };
        let Some(tree) = parse_csharp(&source) else {
            continue;
        };
        parsed.push(ParsedFile {
            file: indexed.path.clone(),
            source,
            tree,
        });
    }
    Ok(parsed)
}

fn emit_class_relations(
    package: &Package,
    parsed: &ParsedFile,
    class: &Class<'_>,
    catalog: &TypeCatalog,
    registrations: &HashMap<String, String>,
    output: &mut FrameworkOutput,
) {
    controller_routes(package, &parsed.file, class, &parsed.source, output);
    emit_injection_relations(package, parsed, class, catalog, registrations, output);
    emit_dbset_relations(package, parsed, class, catalog, output);
}

fn emit_injection_relations(
    package: &Package,
    parsed: &ParsedFile,
    class: &Class<'_>,
    catalog: &TypeCatalog,
    registrations: &HashMap<String, String>,
    output: &mut FrameworkOutput,
) {
    for parameter in &class.constructor_types {
        let Some(implementation) = registrations.get(&parameter.name) else {
            continue;
        };
        let Some(provider) = catalog.unique(implementation) else {
            continue;
        };
        output.relate(FrameworkRelation::new(
            ID,
            Some(&package.id),
            RelationDraft {
                kind: RelationKind::ProviderInjectedInto,
                rule: DI_RULE,
                rule_version: VERSION,
                certainty: Certainty::Declared,
                from: provider,
                to: Some(parsed.file.clone()),
                from_symbol: Some(implementation.clone()),
                to_symbol: Some(class.name.clone()),
                label: implementation.clone(),
                evidence: evidence(&parsed.file, parameter.node, &parsed.source),
            },
        ));
    }
}

fn emit_dbset_relations(
    package: &Package,
    parsed: &ParsedFile,
    class: &Class<'_>,
    catalog: &TypeCatalog,
    output: &mut FrameworkOutput,
) {
    if !class.bases.iter().any(|base| base.ends_with("DbContext")) {
        return;
    }
    for (entity, property, node) in &class.db_sets {
        let Some(entity_file) = catalog.unique(entity) else {
            continue;
        };
        let (label, certainty) = match catalog.tables.get(entity) {
            Some(table) => (table.clone(), Certainty::Declared),
            None => (property.clone(), Certainty::Convention),
        };
        output.relate(FrameworkRelation::new(
            ID,
            Some(&package.id),
            RelationDraft {
                kind: RelationKind::ModelMappedToTable,
                rule: EF_RULE,
                rule_version: VERSION,
                certainty,
                from: entity_file,
                to: None,
                from_symbol: Some(entity.clone()),
                to_symbol: None,
                label,
                evidence: evidence(&parsed.file, *node, &parsed.source),
            },
        ));
    }
}

struct Attribute {
    name: String,
    first_string: Option<String>,
    node_start: usize,
    node_end: usize,
}

struct Parameter<'t> {
    name: String,
    node: Node<'t>,
}

struct Method {
    name: String,
    attributes: Vec<Attribute>,
}

struct Class<'t> {
    name: String,
    attributes: Vec<Attribute>,
    bases: Vec<String>,
    methods: Vec<Method>,
    constructor_types: Vec<Parameter<'t>>,
    db_sets: Vec<(String, String, Node<'t>)>,
}

fn simple_name(node: Node<'_>, source: &str) -> String {
    match node.kind() {
        "generic_name" => node
            .named_child(0)
            .map(|n| text(n, source).to_owned())
            .unwrap_or_default(),
        _ => text(node, source).to_owned(),
    }
}

fn attributes(declaration: Node<'_>, source: &str) -> Vec<Attribute> {
    let mut found = Vec::new();
    let mut cursor = declaration.walk();
    for list in declaration
        .children(&mut cursor)
        .filter(|c| c.kind() == "attribute_list")
    {
        let mut inner = list.walk();
        for attribute in list
            .named_children(&mut inner)
            .filter(|a| a.kind() == "attribute")
        {
            let Some(name) = attribute.child_by_field_name("name") else {
                continue;
            };
            let mut first_string = None;
            walk(attribute, &mut |n| {
                if first_string.is_none() && n.kind() == "string_literal_content" {
                    first_string = Some(text(n, source).to_owned());
                }
            });
            found.push(Attribute {
                name: text(name, source).trim_end_matches("Attribute").to_owned(),
                first_string,
                node_start: attribute.start_byte(),
                node_end: attribute.end_byte(),
            });
        }
    }
    found
}

fn classes<'t>(root: Node<'t>, source: &str) -> Vec<Class<'t>> {
    let mut found = Vec::new();
    walk(root, &mut |node| {
        if node.kind() != "class_declaration" {
            return;
        }
        if let Some(class) = parse_class(node, source) {
            found.push(class);
        }
    });
    found
}

fn parse_class<'t>(node: Node<'t>, source: &str) -> Option<Class<'t>> {
    let name = node.child_by_field_name("name")?;
    let bases = class_bases(node, source);
    let (methods, constructor_types, db_sets) = class_members(node, source);
    Some(Class {
        name: text(name, source).to_owned(),
        attributes: attributes(node, source),
        bases,
        methods,
        constructor_types,
        db_sets,
    })
}

fn class_bases(node: Node<'_>, source: &str) -> Vec<String> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|child| child.kind() == "base_list")
        .flat_map(|list| {
            let mut inner = list.walk();
            list.named_children(&mut inner)
                .map(|base| text(base, source).to_owned())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// A class's methods, constructor parameters and fields (name, type, node).
type ClassMembers<'t> = (
    Vec<Method>,
    Vec<Parameter<'t>>,
    Vec<(String, String, Node<'t>)>,
);

fn class_members<'t>(node: Node<'t>, source: &str) -> ClassMembers<'t> {
    let mut methods = Vec::new();
    let mut constructor_types = Vec::new();
    let mut db_sets = Vec::new();
    let Some(body) = node.child_by_field_name("body") else {
        return (methods, constructor_types, db_sets);
    };
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        match member.kind() {
            "method_declaration" => add_method(member, source, &mut methods),
            "constructor_declaration" => {
                add_constructor_parameters(member, source, &mut constructor_types)
            }
            "property_declaration" => add_db_set(member, source, &mut db_sets),
            _ => {}
        }
    }
    (methods, constructor_types, db_sets)
}

fn add_method<'t>(member: Node<'t>, source: &str, methods: &mut Vec<Method>) {
    let Some(name) = member.child_by_field_name("name") else {
        return;
    };
    methods.push(Method {
        name: text(name, source).to_owned(),
        attributes: attributes(member, source),
    });
}

fn add_constructor_parameters<'t>(
    member: Node<'t>,
    source: &str,
    constructor_types: &mut Vec<Parameter<'t>>,
) {
    let Some(parameters) = member.child_by_field_name("parameters") else {
        return;
    };
    let mut cursor = parameters.walk();
    for parameter in parameters
        .named_children(&mut cursor)
        .filter(|parameter| parameter.kind() == "parameter")
    {
        if let Some(ty) = parameter.child_by_field_name("type") {
            constructor_types.push(Parameter {
                name: simple_name(ty, source),
                node: parameter,
            });
        }
    }
}

fn add_db_set<'t>(member: Node<'t>, source: &str, db_sets: &mut Vec<(String, String, Node<'t>)>) {
    let Some(ty) = member
        .child_by_field_name("type")
        .filter(|ty| ty.kind() == "generic_name")
    else {
        return;
    };
    let Some(property) = member.child_by_field_name("name") else {
        return;
    };
    if ty.named_child(0).map(|name| text(name, source)) != Some("DbSet") {
        return;
    }
    let Some(entity) = ty
        .named_child(1)
        .and_then(|list| list.named_child(0))
        .map(|entity| simple_name(entity, source))
    else {
        return;
    };
    db_sets.push((entity, text(property, source).to_owned(), member));
}

/// Controller templates: `~/x` and `/x` on an action are absolute, anything else extends the prefix.
fn join(prefix: &str, path: &str) -> String {
    if let Some(rooted) = path.strip_prefix("~/").or_else(|| path.strip_prefix('/')) {
        return format!("/{}", rooted.trim_matches('/'));
    }
    join_plain(prefix, path)
}

/// Minimal API patterns and groups always extend the prefix, a leading slash or not.
fn join_plain(prefix: &str, path: &str) -> String {
    let parts: Vec<&str> = [prefix, path]
        .iter()
        .flat_map(|p| p.split('/'))
        .filter(|p| !p.is_empty())
        .collect();
    format!("/{}", parts.join("/"))
}

fn controller_routes(
    package: &Package,
    file: &RootRelativePath,
    class: &Class<'_>,
    source: &str,
    output: &mut FrameworkOutput,
) {
    let is_controller = class.attributes.iter().any(|a| a.name == "ApiController")
        || class
            .bases
            .iter()
            .any(|b| matches!(b.as_str(), "Controller" | "ControllerBase"));
    if !is_controller {
        return;
    }
    let short = class
        .name
        .strip_suffix("Controller")
        .unwrap_or(&class.name)
        .to_owned();
    let class_route = class
        .attributes
        .iter()
        .find(|a| a.name == "Route")
        .and_then(|a| a.first_string.clone())
        .unwrap_or_default();
    let class_expanded = class_route.replace("[controller]", &short);
    for method in &class.methods {
        let route = method
            .attributes
            .iter()
            .find(|a| a.name == "Route")
            .and_then(|a| a.first_string.clone());
        for attribute in &method.attributes {
            let Some((_, verb)) = VERBS.iter().find(|(name, _)| *name == attribute.name) else {
                continue;
            };
            let template = attribute
                .first_string
                .clone()
                .or_else(|| route.clone())
                .unwrap_or_default();
            let token = class_route.contains("[controller]") || template.contains("[action]");
            let full = join(&class_expanded, &template.replace("[action]", &method.name));
            output.relate(FrameworkRelation::new(
                ID,
                Some(&package.id),
                RelationDraft {
                    kind: RelationKind::RouteRegisteredTo,
                    rule: CONTROLLER_RULE,
                    rule_version: VERSION,
                    certainty: if token {
                        Certainty::Convention
                    } else {
                        Certainty::Declared
                    },
                    from: file.clone(),
                    to: Some(file.clone()),
                    from_symbol: Some(class.name.clone()),
                    to_symbol: Some(format!("{}.{}", class.name, method.name)),
                    label: format!("{verb} {full}"),
                    evidence: harness_workspace::framework_relations::RelationEvidence::new(
                        file,
                        harness_workspace::import_graph::ByteSpan {
                            start: attribute.node_start,
                            end: attribute.node_end,
                        },
                        &source[attribute.node_start..attribute.node_end],
                    ),
                },
            ));
        }
    }
}

fn minimal_apis(
    package: &Package,
    file: &RootRelativePath,
    root: Node<'_>,
    source: &str,
    unique: &dyn Fn(&str) -> Option<RootRelativePath>,
    output: &mut FrameworkOutput,
) {
    // `var app = builder.Build();` and `var api = app.MapGroup("/x");` give receivers with prefixes.
    let mut receivers: HashMap<String, String> = HashMap::new();
    walk(root, &mut |node| {
        if node.kind() != "variable_declarator" {
            return;
        }
        let (Some(name), Some(call)) = (
            node.child_by_field_name("name"),
            node.named_children(&mut node.walk())
                .find(|c| c.kind() == "invocation_expression"),
        ) else {
            return;
        };
        let Some(function) = call
            .child_by_field_name("function")
            .filter(|f| f.kind() == "member_access_expression")
        else {
            return;
        };
        let method = function
            .child_by_field_name("name")
            .map(|n| text(n, source))
            .unwrap_or("");
        let object = function
            .child_by_field_name("expression")
            .map(|o| text(o, source))
            .unwrap_or("");
        if method == "Build" {
            receivers.insert(text(name, source).to_owned(), String::new());
        } else if method == "MapGroup" {
            if let Some(parent) = receivers.get(object).cloned() {
                let part = first_literal(call, source).unwrap_or_default();
                receivers.insert(text(name, source).to_owned(), join_plain(&parent, &part));
            }
        }
    });
    if receivers.is_empty() {
        return;
    }
    walk(root, &mut |node| {
        if node.kind() != "invocation_expression" {
            return;
        }
        let Some(function) = node
            .child_by_field_name("function")
            .filter(|f| f.kind() == "member_access_expression")
        else {
            return;
        };
        let method = function
            .child_by_field_name("name")
            .map(|n| text(n, source))
            .unwrap_or("");
        let Some((_, verb)) = MAPS.iter().find(|(name, _)| *name == method) else {
            return;
        };
        let object = function
            .child_by_field_name("expression")
            .map(|o| text(o, source))
            .unwrap_or("");
        let Some(prefix) = receivers.get(object) else {
            return;
        };
        let arguments = argument_nodes(node);
        let (Some(pattern), Some(handler)) = (arguments.first(), arguments.last()) else {
            return;
        };
        let Some(pattern) = first_string(*pattern, source) else {
            output.unresolved(
                file,
                format!(
                    "`{object}.{method}({}, …)` has a computed pattern; no relation emitted",
                    text(*pattern, source)
                ),
            );
            return;
        };
        let (target, symbol) = match handler.kind() {
            "lambda_expression" | "anonymous_method_expression" => (Some(file.clone()), None),
            "member_access_expression" => {
                let class = handler
                    .child_by_field_name("expression")
                    .map(|e| text(e, source).to_owned())
                    .unwrap_or_default();
                (
                    unique(&class),
                    handler
                        .child_by_field_name("name")
                        .map(|n| text(n, source).to_owned()),
                )
            }
            "identifier" => (Some(file.clone()), Some(text(*handler, source).to_owned())),
            _ => (None, None),
        };
        output.relate(FrameworkRelation::new(
            ID,
            Some(&package.id),
            RelationDraft {
                kind: RelationKind::RouteRegisteredTo,
                rule: MINIMAL_RULE,
                rule_version: VERSION,
                certainty: Certainty::Declared,
                from: file.clone(),
                to: target,
                from_symbol: None,
                to_symbol: symbol,
                label: format!("{verb} {}", join_plain(prefix, &pattern)),
                evidence: evidence(file, node, source),
            },
        ));
    });
}

fn argument_nodes<'t>(call: Node<'t>) -> Vec<Node<'t>> {
    let Some(list) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = list.walk();
    list.named_children(&mut cursor)
        .filter_map(|a| a.named_child(0))
        .collect()
}

fn first_string(node: Node<'_>, source: &str) -> Option<String> {
    (node.kind() == "string_literal").then(|| {
        let mut cursor = node.walk();
        let found = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "string_literal_content")
            .map(|c| text(c, source).to_owned())
            .unwrap_or_default();
        found
    })
}

fn first_literal(call: Node<'_>, source: &str) -> Option<String> {
    argument_nodes(call)
        .first()
        .and_then(|a| first_string(*a, source))
}
