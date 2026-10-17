//! Spring: controller routes from mapping annotations, bean injection through constructors,
//! `@Autowired` fields and Lombok's `@RequiredArgsConstructor`, and JPA entities mapped to tables.
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
            for class in classes(p.tree.root_node(), &p.source) {
                routes(package, &p.file, &class, &p.source, output);
                // JPA entity.
                if let Some(entity) = class.annotations.iter().find(|a| a.name == "Entity") {
                    let table = class
                        .annotations
                        .iter()
                        .find(|a| a.name == "Table")
                        .and_then(|t| named_string(t.node, "name", &p.source));
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
                            from: p.file.clone(),
                            to: None,
                            from_symbol: Some(class.name.clone()),
                            to_symbol: None,
                            label,
                            evidence: evidence(&p.file, entity.node, &p.source),
                        },
                    ));
                }
                // Injection.
                if !class
                    .annotations
                    .iter()
                    .any(|a| BEANS.contains(&a.name.as_str()))
                {
                    continue;
                }
                for injected in injected_types(&class, &p.source) {
                    let provider = bindings.get(&injected.type_name).cloned().or_else(|| {
                        types
                            .get(&(package_name.clone(), injected.type_name.clone()))
                            .filter(|(_, is)| *is)
                            .map(|(f, _)| f.clone())
                    });
                    let Some(provider) = provider else { continue };
                    // The type must be a bean: look it up by name in the file it resolved to.
                    let is_bean = types.iter().any(|((_, name), (file, is))| {
                        *name == injected.type_name && *file == provider && *is
                    });
                    if !is_bean {
                        continue;
                    }
                    output.relate(FrameworkRelation::new(
                        ID,
                        Some(&package.id),
                        RelationDraft {
                            kind: RelationKind::ProviderInjectedInto,
                            rule: INJECTION_RULE,
                            rule_version: VERSION,
                            certainty: Certainty::Declared,
                            from: provider,
                            to: Some(p.file.clone()),
                            from_symbol: Some(injected.type_name.clone()),
                            to_symbol: Some(class.name.clone()),
                            label: injected.type_name,
                            evidence: evidence(&p.file, injected.node, &p.source),
                        },
                    ));
                }
            }
        }
        Ok(())
    }
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
    if !class
        .annotations
        .iter()
        .any(|a| matches!(a.name.as_str(), "RestController" | "Controller"))
    {
        return;
    }
    let prefixes = match class
        .annotations
        .iter()
        .find(|a| a.name == "RequestMapping")
    {
        None => vec![String::new()],
        Some(mapping) => match mapping_paths(mapping.node, source) {
            Some(paths) if paths.is_empty() => vec![String::new()],
            Some(paths) => paths,
            None => {
                output.unresolved(
                    file,
                    format!(
                        "`{}` has a computed @RequestMapping path; its routes were not read",
                        class.name
                    ),
                );
                return;
            }
        },
    };
    let Some(body) = class.node.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    for member in body
        .named_children(&mut cursor)
        .filter(|m| m.kind() == "method_declaration")
    {
        let method_name = member
            .child_by_field_name("name")
            .map(|n| text(n, source).to_owned())
            .unwrap_or_default();
        for annotation in annotations(member, source) {
            let verbs: Vec<String> = match annotation.name.as_str() {
                "GetMapping" => vec!["GET".into()],
                "PostMapping" => vec!["POST".into()],
                "PutMapping" => vec!["PUT".into()],
                "DeleteMapping" => vec!["DELETE".into()],
                "PatchMapping" => vec!["PATCH".into()],
                "RequestMapping" => request_methods(annotation.node, source),
                _ => continue,
            };
            let paths = match mapping_paths(annotation.node, source) {
                Some(paths) if paths.is_empty() => vec![String::new()],
                Some(paths) => paths,
                None => {
                    output.unresolved(
                        file,
                        format!(
                            "`{}.{method_name}` has a computed @{} path; no relation emitted",
                            class.name, annotation.name
                        ),
                    );
                    continue;
                }
            };
            for prefix in &prefixes {
                for path in &paths {
                    for verb in &verbs {
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
                                from_symbol: Some(class.name.clone()),
                                to_symbol: Some(format!("{}.{method_name}", class.name)),
                                label: format!("{verb} {}", join(prefix, path)),
                                evidence: evidence(file, annotation.node, source),
                            },
                        ));
                    }
                }
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
    let mut found = Vec::new();
    let Some(body) = class.node.child_by_field_name("body") else {
        return found;
    };
    let lombok = class
        .annotations
        .iter()
        .any(|a| a.name == "RequiredArgsConstructor");
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        match member.kind() {
            "constructor_declaration" => {
                if let Some(parameters) = member.child_by_field_name("parameters") {
                    let mut inner = parameters.walk();
                    for parameter in parameters
                        .named_children(&mut inner)
                        .filter(|p| p.kind() == "formal_parameter")
                    {
                        if let Some(name) = type_name(parameter, source) {
                            found.push(Injected {
                                type_name: name,
                                node: parameter,
                            });
                        }
                    }
                }
            }
            "field_declaration" => {
                let modifiers = member
                    .children(&mut member.walk())
                    .find(|c| c.kind() == "modifiers")
                    .map(|m| text(m, source).to_owned())
                    .unwrap_or_default();
                let wired = annotations(member, source)
                    .iter()
                    .any(|a| matches!(a.name.as_str(), "Autowired" | "Inject" | "Resource"));
                let final_field =
                    lombok && modifiers.contains("final") && !modifiers.contains("static");
                if wired || final_field {
                    if let Some(name) = type_name(member, source) {
                        found.push(Injected {
                            type_name: name,
                            node: member,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    found
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
