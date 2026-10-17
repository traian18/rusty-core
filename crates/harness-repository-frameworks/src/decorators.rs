//! Classes, decorators and constructor parameters, shared by the decorator-based frameworks
//! (NestJS and Angular).
use harness_workspace::{
    framework_relations::{
        Certainty, FrameworkInput, FrameworkOutput, FrameworkRelation, RelationDraft, RelationKind,
    },
    repository_intelligence::{Package, RootRelativePath},
};
use tree_sitter::Node;

use crate::syntax::{evidence, is_script, parse_script, text, walk};

/// `@Name(args)` or `@Name`.
pub struct Decorator<'t> {
    pub name: String,
    pub arguments: Option<Node<'t>>,
    pub node: Node<'t>,
}

pub struct Parameter<'t> {
    /// The type written on the parameter, when it is a plain name.
    pub type_name: Option<String>,
    /// Decorators on the parameter (`@Inject('TOKEN')`).
    pub decorators: Vec<Decorator<'t>>,
    pub node: Node<'t>,
}

pub struct Method<'t> {
    pub name: String,
    pub decorators: Vec<Decorator<'t>>,
    pub parameters: Vec<Parameter<'t>>,
}

pub struct Class<'t> {
    pub name: String,
    pub decorators: Vec<Decorator<'t>>,
    pub methods: Vec<Method<'t>>,
    pub node: Node<'t>,
}

impl Class<'_> {
    pub fn decorator(&self, name: &str) -> Option<&Decorator<'_>> {
        self.decorators.iter().find(|d| d.name == name)
    }

    pub fn constructor(&self) -> Option<&Method<'_>> {
        self.methods.iter().find(|m| m.name == "constructor")
    }
}

fn decorator<'t>(node: Node<'t>, source: &str) -> Option<Decorator<'t>> {
    let inner = node.named_child(0)?;
    match inner.kind() {
        "call_expression" => {
            let function = inner.child_by_field_name("function")?;
            Some(Decorator {
                name: text(function, source).rsplit('.').next()?.to_owned(),
                arguments: inner.child_by_field_name("arguments"),
                node,
            })
        }
        "identifier" => Some(Decorator {
            name: text(inner, source).to_owned(),
            arguments: None,
            node,
        }),
        _ => None,
    }
}

fn decorators_of<'t>(node: Node<'t>, source: &str) -> Vec<Decorator<'t>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|child| child.kind() == "decorator")
        .filter_map(|child| decorator(child, source))
        .collect()
}

fn parameter<'t>(node: Node<'t>, source: &str) -> Parameter<'t> {
    let type_name = node
        .child_by_field_name("type")
        .and_then(|annotation| annotation.named_child(0))
        .filter(|ty| matches!(ty.kind(), "type_identifier" | "identifier"))
        .map(|ty| text(ty, source).to_owned());
    Parameter {
        type_name,
        decorators: decorators_of(node, source),
        node,
    }
}

fn method<'t>(node: Node<'t>, source: &str) -> Option<Method<'t>> {
    let name = text(node.child_by_field_name("name")?, source).to_owned();
    let mut parameters = Vec::new();
    if let Some(list) = node.child_by_field_name("parameters") {
        let mut cursor = list.walk();
        for child in list.named_children(&mut cursor) {
            if matches!(child.kind(), "required_parameter" | "optional_parameter") {
                parameters.push(parameter(child, source));
            }
        }
    }
    Some(Method {
        name,
        decorators: decorators_of(node, source),
        parameters,
    })
}

/// Every named class in the tree with its decorators (also those written before `export`).
pub fn classes<'t>(root: Node<'t>, source: &str) -> Vec<Class<'t>> {
    let mut found = Vec::new();
    walk(root, &mut |node| {
        if node.kind() != "class_declaration" {
            return;
        }
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let mut decorators = decorators_of(node, source);
        if let Some(parent) = node.parent().filter(|p| p.kind() == "export_statement") {
            decorators.extend(decorators_of(parent, source));
        }
        let mut methods = Vec::new();
        if let Some(body) = node.child_by_field_name("body") {
            // A decorator on a member is a sibling that comes before it in the class body.
            let mut pending = Vec::new();
            let mut cursor = body.walk();
            for member in body.named_children(&mut cursor) {
                match member.kind() {
                    "decorator" => pending.extend(decorator(member, source)),
                    "method_definition" => {
                        if let Some(mut found) = method(member, source) {
                            pending.append(&mut found.decorators);
                            found.decorators = std::mem::take(&mut pending);
                            methods.push(found);
                        }
                    }
                    _ => pending.clear(),
                }
            }
        }
        found.push(Class {
            name: text(name, source).to_owned(),
            decorators,
            methods,
            node,
        });
    });
    found
}

/// The first string literal among a decorator's arguments, whether bare or as `{ path: '…' }`.
pub enum PathArgument {
    /// No argument: the empty path.
    None,
    Literal(String),
    /// Present but not a literal.
    Computed,
}

pub fn path_argument(decorator: &Decorator<'_>, source: &str) -> PathArgument {
    let Some(arguments) = decorator.arguments else {
        return PathArgument::None;
    };
    let Some(first) = arguments.named_child(0) else {
        return PathArgument::None;
    };
    if let Some(value) = crate::syntax::string_value(first, source) {
        return PathArgument::Literal(value);
    }
    if first.kind() == "object" {
        let mut cursor = first.walk();
        for pair in first.named_children(&mut cursor) {
            if pair.kind() == "pair" {
                let key = pair
                    .child_by_field_name("key")
                    .map(|k| text(k, source).trim_matches(['\'', '"']).to_owned());
                if key.as_deref() == Some("path") {
                    return match pair
                        .child_by_field_name("value")
                        .and_then(|v| crate::syntax::string_value(v, source))
                    {
                        Some(value) => PathArgument::Literal(value),
                        None => PathArgument::Computed,
                    };
                }
            }
        }
        return PathArgument::None;
    }
    PathArgument::Computed
}

/// How a framework spells provider injection.
pub struct InjectionRules {
    pub framework: &'static str,
    pub rule: &'static str,
    pub version: u32,
    /// The decorator that marks a class as injectable.
    pub injectable: &'static str,
    /// Decorators whose classes receive injected constructor parameters.
    pub consumers: &'static [&'static str],
    /// A function that injects by type when called (Angular's `inject(Service)`), if the framework has one.
    pub inject_function: Option<&'static str>,
}

/// One class's injectable and consumer facts, kept for the second pass.
struct Known<'t> {
    class: Class<'t>,
}

/// Provider-injection relations for one package: a constructor parameter typed with an
/// `@Injectable` class defined in the package becomes `provider_injected_into`. Parameters with an
/// explicit `@Inject(...)` token are reported, not followed.
pub fn injections(
    package: &Package,
    input: &FrameworkInput<'_>,
    rules: &InjectionRules,
    output: &mut FrameworkOutput,
) {
    struct Parsed {
        file: RootRelativePath,
        source: String,
        tree: tree_sitter::Tree,
    }
    let mut parsed = Vec::new();
    for indexed in input.files_of(package) {
        if !is_script(&indexed.path) {
            continue;
        }
        let Some(source) = input.read(&indexed.path) else {
            continue;
        };
        if !source.contains('@') && !source.contains("inject(") {
            continue;
        }
        let Some(tree) = parse_script(&indexed.path, &source) else {
            continue;
        };
        parsed.push(Parsed {
            file: indexed.path.clone(),
            source,
            tree,
        });
    }
    let per_file: Vec<(&RootRelativePath, &str, Vec<Known<'_>>)> = parsed
        .iter()
        .map(|p| {
            (
                &p.file,
                p.source.as_str(),
                classes(p.tree.root_node(), &p.source)
                    .into_iter()
                    .map(|class| Known { class })
                    .collect(),
            )
        })
        .collect();
    let is_injectable = |file: &RootRelativePath, name: &str| {
        per_file.iter().any(|(path, _, known)| {
            *path == file
                && known
                    .iter()
                    .any(|k| k.class.name == name && k.class.decorator(rules.injectable).is_some())
        })
    };
    for (file, source, known) in &per_file {
        for item in known {
            let class = &item.class;
            if !rules.consumers.iter().any(|d| class.decorator(d).is_some()) {
                continue;
            }
            // Constructor parameters, then `inject(Type)` calls anywhere in the class body.
            let mut injected: Vec<(Option<String>, Node<'_>, bool)> = Vec::new();
            if let Some(constructor) = class.constructor() {
                for parameter in &constructor.parameters {
                    let explicit = parameter.decorators.iter().any(|d| d.name == "Inject");
                    injected.push((parameter.type_name.clone(), parameter.node, explicit));
                }
            }
            if let Some(function) = rules.inject_function {
                walk(class.node, &mut |node| {
                    if node.kind() != "call_expression" {
                        return;
                    }
                    let Some(callee) = node.child_by_field_name("function") else {
                        return;
                    };
                    if text(callee, source) != function {
                        return;
                    }
                    let argument = node
                        .child_by_field_name("arguments")
                        .and_then(|a| a.named_child(0));
                    match argument {
                        Some(argument) if argument.kind() == "identifier" => {
                            injected.push((Some(text(argument, source).to_owned()), node, false));
                        }
                        _ => injected.push((None, node, true)),
                    }
                });
            }
            for (type_name, node, explicit) in injected {
                if explicit {
                    output.unresolved(
                        file,
                        format!("`{}` injects with an explicit or computed token (`{}`); no relation emitted", class.name, text(node, source)),
                    );
                    continue;
                }
                let Some(type_name) = &type_name else {
                    continue;
                };
                let provider_file = if per_file.iter().any(|(path, _, k)| {
                    path == file && k.iter().any(|c| c.class.name == *type_name)
                }) {
                    Some((*file).clone())
                } else {
                    input.names.target(file, type_name).cloned()
                };
                let Some(provider_file) = provider_file else {
                    continue;
                };
                if !is_injectable(&provider_file, type_name) {
                    continue;
                }
                output.relate(FrameworkRelation::new(
                    rules.framework,
                    Some(&package.id),
                    RelationDraft {
                        kind: RelationKind::ProviderInjectedInto,
                        rule: rules.rule,
                        rule_version: rules.version,
                        certainty: Certainty::Declared,
                        from: provider_file,
                        to: Some((*file).clone()),
                        from_symbol: Some(type_name.clone()),
                        to_symbol: Some(class.name.clone()),
                        label: type_name.clone(),
                        evidence: evidence(file, node, source),
                    },
                ));
            }
        }
    }
}
