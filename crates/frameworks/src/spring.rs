//! Spring: controller routes from mapping annotations, bean injection through constructors,
//! `@Autowired` fields and Lombok's `@RequiredArgsConstructor`, and JPA entities mapped to tables.
use std::collections::HashMap;

use rusty_repo_model::{
    framework_relations::{
        Certainty, FrameworkAdapter, FrameworkCapability, FrameworkInput, FrameworkOutput,
        FrameworkRelation, RelationDraft, RelationKind,
    },
    repository_intelligence::{DiscoveryError, Package, RootRelativePath},
};
use tokio_util::sync::CancellationToken;
use tree_sitter::Node;

use crate::syntax::{evidence, parse_java, text, walk};

const ID: &str = "spring";
const VERSION: u32 = 1;
const ROUTE_RULE: &str = "spring-mapping-annotation";
const INJECTION_RULE: &str = "spring-bean-injection";
const ENTITY_RULE: &str = "spring-jpa-entity";
const BEANS: [&str; 6] = [
    "Component",
    "Service",
    "Repository",
    "Configuration",
    "RestController",
    "Controller",
];
const PROVIDERS: [&str; 3] = ["Component", "Service", "Repository"];
const REPOSITORY_BASES: [&str; 5] = [
    "JpaRepository",
    "CrudRepository",
    "Repository",
    "PagingAndSortingRepository",
    "MongoRepository",
];

pub struct SpringAdapter;

impl FrameworkAdapter for SpringAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "Spring Boot".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::RouteRegisteredTo, RelationKind::ProviderInjectedInto, RelationKind::ModelMappedToTable],
            handled: vec![
                "@RestController/@Controller classes with @RequestMapping (class level) and @GetMapping, @PostMapping, @PutMapping, @DeleteMapping, @PatchMapping or @RequestMapping(method = …) on methods; literal paths and path arrays".into(),
                "bean injection: constructor parameters, @Autowired/@Inject fields and final fields under @RequiredArgsConstructor, when the type is a @Component/@Service/@Repository class or a Spring Data repository interface of the repository".into(),
                "@Entity classes: the table is @Table(name = \"…\") when literal (declared), otherwise the class name in snake case (convention)".into(),
            ],
            unsupported: vec![
                "@Bean methods, @Qualifier and @Primary selection, profiles and conditional beans, injection of interfaces implemented by several beans".into(),
                "paths built from constants or concatenation (reported, no relation); context paths and spring.mvc settings".into(),
                "Kotlin and Groovy sources, XML configuration".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        package
            .facts
            .declared_dependencies
            .iter()
            .find(|d| d.starts_with("org.springframework"))
            .map(|d| format!("declares dependency `{d}`"))
    }

    fn extract(
        &self,
        package: &Package,
        input: &FrameworkInput<'_>,
        cancel: &CancellationToken,
        output: &mut FrameworkOutput,
    ) -> Result<(), DiscoveryError> {
        struct Parsed {
            file: RootRelativePath,
            source: String,
            tree: tree_sitter::Tree,
        }
        let mut parsed = Vec::new();
        for indexed in input.files_of(package) {
            if cancel.is_cancelled() {
                return Err(DiscoveryError::Cancelled);
            }
            if indexed.path.as_path().extension().and_then(|e| e.to_str()) != Some("java") {
                continue;
            }
            let Some(source) = input.read(&indexed.path) else {
                continue;
            };
            let Some(tree) = parse_java(&source) else {
                continue;
            };
            parsed.push(Parsed {
                file: indexed.path.clone(),
                source,
                tree,
            });
        }
        // (package, type name) -> (file, can be injected), for every type declared in the package.
        let mut types: HashMap<(String, String), (RootRelativePath, bool)> = HashMap::new();
        let mut file_package: HashMap<&RootRelativePath, String> = HashMap::new();
        for p in &parsed {
            let package_name = java_package(p.tree.root_node(), &p.source);
            file_package.insert(&p.file, package_name.clone());
            for class in classes(p.tree.root_node(), &p.source) {
                let provider = class
                    .annotations
                    .iter()
                    .any(|a| PROVIDERS.contains(&a.name.as_str()))
                    || class.extends_repository;
                types
                    .entry((package_name.clone(), class.name.clone()))
                    .or_insert((p.file.clone(), provider));
            }
        }
        for p in &parsed {
            let package_name = file_package.get(&p.file).cloned().unwrap_or_default();
            let bindings = import_bindings(input, &p.file);
            let context = ClassContext {
                package,
                file: &p.file,
                source: &p.source,
                package_name: &package_name,
                bindings: &bindings,
                types: &types,
            };
            for class in classes(p.tree.root_node(), &p.source) {
                process_class(&context, &class, output);
            }
        }
        Ok(())
    }
}

struct ClassContext<'a> {
    package: &'a Package,
    file: &'a RootRelativePath,
    source: &'a str,
    package_name: &'a str,
    bindings: &'a HashMap<String, RootRelativePath>,
    types: &'a HashMap<(String, String), (RootRelativePath, bool)>,
}

fn process_class(context: &ClassContext<'_>, class: &Class<'_>, output: &mut FrameworkOutput) {
    routes(context.package, context.file, class, context.source, output);
    emit_entity_relation(context.package, context.file, context.source, class, output);
    emit_injection_relations(context, class, output);
}

fn emit_entity_relation(
    package: &Package,
    file: &RootRelativePath,
    source: &str,
    class: &Class<'_>,
    output: &mut FrameworkOutput,
) {
    let Some(entity) = class
        .annotations
        .iter()
        .find(|annotation| annotation.name == "Entity")
    else {
        return;
    };
    let table = class
        .annotations
        .iter()
        .find(|annotation| annotation.name == "Table")
        .and_then(|annotation| named_string(annotation.node, "name", source));
    let (label, certainty) = match table {
        Some(name) => (name, Certainty::Declared),
        None => (snake_case(&class.name), Certainty::Convention),
    };
    output.relate(FrameworkRelation::new(
        ID,
        Some(&package.id),
        RelationDraft {
            kind: RelationKind::ModelMappedToTable,
            rule: ENTITY_RULE,
            rule_version: VERSION,
            certainty,
            from: file.clone(),
            to: None,
            from_symbol: Some(class.name.clone()),
            to_symbol: None,
            label,
            evidence: evidence(file, entity.node, source),
        },
    ));
}

fn emit_injection_relations(
    context: &ClassContext<'_>,
    class: &Class<'_>,
    output: &mut FrameworkOutput,
) {
    if !is_bean_class(class) {
        return;
    }
    for injected in injected_types(class, context.source) {
        let Some(provider) = resolve_provider(
            context.package_name,
            context.bindings,
            context.types,
            &injected.type_name,
        ) else {
            continue;
        };
        output.relate(FrameworkRelation::new(
            ID,
            Some(&context.package.id),
            RelationDraft {
                kind: RelationKind::ProviderInjectedInto,
                rule: INJECTION_RULE,
                rule_version: VERSION,
                certainty: Certainty::Declared,
                from: provider,
                to: Some(context.file.clone()),
                from_symbol: Some(injected.type_name.clone()),
                to_symbol: Some(class.name.clone()),
                label: injected.type_name,
                evidence: evidence(context.file, injected.node, context.source),
            },
        ));
    }
}

fn is_bean_class(class: &Class<'_>) -> bool {
    class
        .annotations
        .iter()
        .any(|annotation| BEANS.contains(&annotation.name.as_str()))
}

fn resolve_provider(
    package_name: &str,
    bindings: &HashMap<String, RootRelativePath>,
    types: &HashMap<(String, String), (RootRelativePath, bool)>,
    type_name: &str,
) -> Option<RootRelativePath> {
    let provider = bindings.get(type_name).cloned().or_else(|| {
        types
            .get(&(package_name.to_owned(), type_name.to_owned()))
            .filter(|(_, is_provider)| *is_provider)
            .map(|(file, _)| file.clone())
    })?;
    let is_bean = types.iter().any(|((_, name), (file, is_provider))| {
        name == type_name && *file == provider && *is_provider
    });
    is_bean.then_some(provider)
}

fn java_package(root: Node<'_>, source: &str) -> String {
    let mut cursor = root.walk();
    let found = root
        .children(&mut cursor)
        .find(|c| c.kind() == "package_declaration")
        .and_then(|c| c.named_child(0))
        .map(|n| text(n, source).to_owned());
    found.unwrap_or_default()
}

/// Imported simple name -> the file its import resolved to.
fn import_bindings(
    input: &FrameworkInput<'_>,
    file: &RootRelativePath,
) -> HashMap<String, RootRelativePath> {
    let mut found = HashMap::new();
    for edge in input
        .imports
        .edges
        .iter()
        .filter(|e| &e.fact.importing_file == file && !e.fact.glob)
    {
        if let Some(target) = edge.resolution.target_file() {
            let simple = edge
                .fact
                .spelling
                .rsplit('.')
                .next()
                .unwrap_or("")
                .to_owned();
            found.entry(simple).or_insert_with(|| target.clone());
        }
    }
    found
}

struct Annotation<'t> {
    name: String,
    node: Node<'t>,
}

struct Class<'t> {
    name: String,
    annotations: Vec<Annotation<'t>>,
    extends_repository: bool,
    node: Node<'t>,
}

fn annotations<'t>(declaration: Node<'t>, source: &str) -> Vec<Annotation<'t>> {
    let mut found = Vec::new();
    let mut cursor = declaration.walk();
    for child in declaration
        .children(&mut cursor)
        .filter(|c| c.kind() == "modifiers")
    {
        let mut inner = child.walk();
        for annotation in child
            .children(&mut inner)
            .filter(|a| matches!(a.kind(), "marker_annotation" | "annotation"))
        {
            if let Some(name) = annotation.child_by_field_name("name") {
                found.push(Annotation {
                    name: text(name, source)
                        .rsplit('.')
                        .next()
                        .unwrap_or("")
                        .to_owned(),
                    node: annotation,
                });
            }
        }
    }
    found
}

fn classes<'t>(root: Node<'t>, source: &str) -> Vec<Class<'t>> {
    let mut found = Vec::new();
    walk(root, &mut |node| {
        if !matches!(
            node.kind(),
            "class_declaration" | "interface_declaration" | "record_declaration"
        ) {
            return;
        }
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let header = source[node.start_byte()
            ..node
                .child_by_field_name("body")
                .map_or(node.end_byte(), |b| b.start_byte())]
            .to_owned();
        let extends_repository = node.kind() == "interface_declaration"
            && REPOSITORY_BASES.iter().any(|b| {
                header.contains(&format!("{b}<")) || header.contains(&format!("extends {b}"))
            });
        found.push(Class {
            name: text(name, source).to_owned(),
            annotations: annotations(node, source),
            extends_repository,
            node,
        });
    });
    found
}

/// Strings written in an annotation argument: `"/a"`, `{"/a", "/b"}`, or `name = "/a"`.
/// `None` when a part is not a literal.
fn literal_strings(value: Node<'_>, source: &str) -> Option<Vec<String>> {
    match value.kind() {
        "string_literal" => Some(vec![text(value, source).trim_matches('"').to_owned()]),
        "element_value_array_initializer" | "array_initializer" => {
            let mut cursor = value.walk();
            let mut out = Vec::new();
            for item in value.named_children(&mut cursor) {
                out.extend(literal_strings(item, source)?);
            }
            Some(out)
        }
        _ => None,
    }
}

/// Paths of a mapping annotation: `Some(vec![])` for none given, `None` when computed.
fn mapping_paths(annotation: Node<'_>, source: &str) -> Option<Vec<String>> {
    let Some(arguments) = annotation.child_by_field_name("arguments") else {
        return Some(Vec::new());
    };
    let mut cursor = arguments.walk();
    for argument in arguments.named_children(&mut cursor) {
        if argument.kind() == "element_value_pair" {
            let key = argument.child_by_field_name("key").map(|k| text(k, source));
            if matches!(key, Some("value" | "path")) {
                return literal_strings(argument.child_by_field_name("value")?, source);
            }
        } else {
            return literal_strings(argument, source);
        }
    }
    Some(Vec::new())
}

fn named_string(annotation: Node<'_>, key: &str, source: &str) -> Option<String> {
    let arguments = annotation.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    for argument in arguments.named_children(&mut cursor) {
        if argument.kind() == "element_value_pair"
            && argument.child_by_field_name("key").map(|k| text(k, source)) == Some(key)
        {
            return literal_strings(argument.child_by_field_name("value")?, source)?
                .into_iter()
                .next();
        }
    }
    None
}

fn request_methods(annotation: Node<'_>, source: &str) -> Vec<String> {
    let mut methods = Vec::new();
    walk(annotation, &mut |n| {
        if n.kind() == "element_value_pair"
            && n.child_by_field_name("key").map(|k| text(k, source)) == Some("method")
        {
            walk(n, &mut |inner| {
                if inner.kind() == "field_access" || inner.kind() == "scoped_identifier" {
                    if let Some(name) = text(inner, source).strip_prefix("RequestMethod.") {
                        methods.push(name.to_owned());
                    }
                }
            });
        }
    });
    if methods.is_empty() {
        vec!["ANY".into()]
    } else {
        methods
    }
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
    class: &Class<'_>,
    source: &str,
    output: &mut FrameworkOutput,
) {
    if !is_controller(class) {
        return;
    }
    let Some(prefixes) = class_route_prefixes(class, file, source, output) else {
        return;
    };
    let Some(body) = class.node.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    for member in body
        .named_children(&mut cursor)
        .filter(|member| member.kind() == "method_declaration")
    {
        let method_name = member
            .child_by_field_name("name")
            .map(|name| text(name, source).to_owned())
            .unwrap_or_default();
        for annotation in annotations(member, source) {
            let Some(verbs) = mapping_verbs(&annotation, source) else {
                continue;
            };
            let Some(paths) =
                method_route_paths(&annotation, file, &class.name, &method_name, source, output)
            else {
                continue;
            };
            let route = RouteContext {
                package,
                file,
                class,
                method_name: &method_name,
                annotation: &annotation,
                source,
            };
            emit_routes(&route, &prefixes, &paths, &verbs, output);
        }
    }
}

fn is_controller(class: &Class<'_>) -> bool {
    class
        .annotations
        .iter()
        .any(|annotation| matches!(annotation.name.as_str(), "RestController" | "Controller"))
}

fn class_route_prefixes(
    class: &Class<'_>,
    file: &RootRelativePath,
    source: &str,
    output: &mut FrameworkOutput,
) -> Option<Vec<String>> {
    let mapping = class
        .annotations
        .iter()
        .find(|annotation| annotation.name == "RequestMapping");
    let Some(mapping) = mapping else {
        return Some(vec![String::new()]);
    };
    match mapping_paths(mapping.node, source) {
        Some(paths) => Some(default_empty_path(paths)),
        None => {
            output.unresolved(
                file,
                format!(
                    "`{}` has a computed @RequestMapping path; its routes were not read",
                    class.name
                ),
            );
            None
        }
    }
}

fn default_empty_path(paths: Vec<String>) -> Vec<String> {
    if paths.is_empty() {
        vec![String::new()]
    } else {
        paths
    }
}

fn mapping_verbs(annotation: &Annotation<'_>, source: &str) -> Option<Vec<String>> {
    match annotation.name.as_str() {
        "GetMapping" => Some(vec!["GET".into()]),
        "PostMapping" => Some(vec!["POST".into()]),
        "PutMapping" => Some(vec!["PUT".into()]),
        "DeleteMapping" => Some(vec!["DELETE".into()]),
        "PatchMapping" => Some(vec!["PATCH".into()]),
        "RequestMapping" => Some(request_methods(annotation.node, source)),
        _ => None,
    }
}

fn method_route_paths(
    annotation: &Annotation<'_>,
    file: &RootRelativePath,
    class_name: &str,
    method_name: &str,
    source: &str,
    output: &mut FrameworkOutput,
) -> Option<Vec<String>> {
    match mapping_paths(annotation.node, source) {
        Some(paths) => Some(default_empty_path(paths)),
        None => {
            output.unresolved(
                file,
                format!(
                    "`{class_name}.{method_name}` has a computed @{} path; no relation emitted",
                    annotation.name
                ),
            );
            None
        }
    }
}

struct RouteContext<'a, 't> {
    package: &'a Package,
    file: &'a RootRelativePath,
    class: &'a Class<'t>,
    method_name: &'a str,
    annotation: &'a Annotation<'t>,
    source: &'a str,
}

fn emit_routes(
    route: &RouteContext<'_, '_>,
    prefixes: &[String],
    paths: &[String],
    verbs: &[String],
    output: &mut FrameworkOutput,
) {
    for prefix in prefixes {
        for path in paths {
            for verb in verbs {
                output.relate(FrameworkRelation::new(
                    ID,
                    Some(&route.package.id),
                    RelationDraft {
                        kind: RelationKind::RouteRegisteredTo,
                        rule: ROUTE_RULE,
                        rule_version: VERSION,
                        certainty: Certainty::Declared,
                        from: route.file.clone(),
                        to: Some(route.file.clone()),
                        from_symbol: Some(route.class.name.clone()),
                        to_symbol: Some(format!("{}.", route.class.name) + route.method_name),
                        label: format!("{verb} {}", join(prefix, path)),
                        evidence: evidence(route.file, route.annotation.node, route.source),
                    },
                ));
            }
        }
    }
}

struct Injected<'t> {
    type_name: String,
    node: Node<'t>,
}

/// Types a bean class receives: constructor parameters, `@Autowired`/`@Inject` fields, and final
/// fields when Lombok's `@RequiredArgsConstructor` writes the constructor.
fn injected_types<'t>(class: &Class<'t>, source: &str) -> Vec<Injected<'t>> {
    let Some(body) = class.node.child_by_field_name("body") else {
        return Vec::new();
    };
    let lombok = has_required_args_constructor(class);
    let mut cursor = body.walk();
    body.named_children(&mut cursor)
        .flat_map(|member| injected_member_types(member, source, lombok))
        .collect()
}

fn has_required_args_constructor(class: &Class<'_>) -> bool {
    class
        .annotations
        .iter()
        .any(|annotation| annotation.name == "RequiredArgsConstructor")
}

fn injected_member_types<'t>(member: Node<'t>, source: &str, lombok: bool) -> Vec<Injected<'t>> {
    match member.kind() {
        "constructor_declaration" => constructor_injected_types(member, source),
        "field_declaration" => field_injected_type(member, source, lombok)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn constructor_injected_types<'t>(constructor: Node<'t>, source: &str) -> Vec<Injected<'t>> {
    let Some(parameters) = constructor.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut cursor = parameters.walk();
    parameters
        .named_children(&mut cursor)
        .filter(|parameter| parameter.kind() == "formal_parameter")
        .filter_map(|parameter| {
            type_name(parameter, source).map(|type_name| Injected {
                type_name,
                node: parameter,
            })
        })
        .collect()
}

fn field_injected_type<'t>(field: Node<'t>, source: &str, lombok: bool) -> Option<Injected<'t>> {
    let modifiers = field
        .children(&mut field.walk())
        .find(|child| child.kind() == "modifiers")
        .map(|modifiers| text(modifiers, source))
        .unwrap_or_default();
    let explicitly_wired = annotations(field, source).iter().any(|annotation| {
        matches!(
            annotation.name.as_str(),
            "Autowired" | "Inject" | "Resource"
        )
    });
    let lombok_injected = lombok && modifiers.contains("final") && !modifiers.contains("static");
    if !explicitly_wired && !lombok_injected {
        return None;
    }
    Some(Injected {
        type_name: type_name(field, source)?,
        node: field,
    })
}

/// The simple name of a declaration's type: `UserService`, or `Repo` for `Repo<User>`.
fn type_name(declaration: Node<'_>, source: &str) -> Option<String> {
    let ty = declaration.child_by_field_name("type")?;
    let simple = match ty.kind() {
        "type_identifier" => ty,
        "generic_type" => ty
            .named_child(0)
            .filter(|n| n.kind() == "type_identifier")?,
        _ => return None,
    };
    Some(text(simple, source).to_owned())
}

/// `UserAccount` becomes `user_account`.
fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}
