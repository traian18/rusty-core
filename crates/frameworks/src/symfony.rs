//! Symfony: routes from `#[Route]` attributes and YAML route files, Doctrine entities and their
//! tables, `#[AsCommand]` console commands, and constructor autowiring of repository classes.
use std::collections::HashMap;

use rusty_repo_model::{
    framework_relations::{
        declares, Certainty, FrameworkAdapter, FrameworkCapability, FrameworkInput,
        FrameworkOutput, FrameworkRelation, RelationDraft, RelationEvidence, RelationKind,
    },
    import_graph::ByteSpan,
    repository_intelligence::{DiscoveryError, Package, RootRelativePath},
};
use tokio_util::sync::CancellationToken;
use tree_sitter::Node;

use crate::{
    php_common::{class_index, classes, php_files, snake_case, string_value, Attribute},
    syntax::{evidence, parse_php, text, walk},
};

const ID: &str = "symfony";
const VERSION: u32 = 1;
const ROUTE_RULE: &str = "symfony-route-attribute";
const YAML_RULE: &str = "symfony-route-yaml";
const ENTITY_RULE: &str = "doctrine-entity-table";
const COMMAND_RULE: &str = "symfony-as-command";
const AUTOWIRE_RULE: &str = "symfony-constructor-autowiring";

pub struct SymfonyAdapter;

impl FrameworkAdapter for SymfonyAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "Symfony".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::RouteRegisteredTo, RelationKind::ProviderInjectedInto, RelationKind::ModelMappedToTable, RelationKind::CommandExposedAs],
            handled: vec![
                "#[Route('/path', methods: [...])] on controller methods with a #[Route] prefix on the class; literal paths only".into(),
                "config/routes.yaml and config/routes/*.yaml entries with a path and a controller (Class::method or an invokable Class)".into(),
                "#[ORM\\Entity] classes: #[ORM\\Table(name: '…')] when literal (declared), otherwise the snake-case class name (convention)".into(),
                "#[AsCommand(name: '…')] classes".into(),
                "constructor parameters of classes under src/ that name a class of the repository (autowired by the container, a convention)".into(),
            ],
            unsupported: vec![
                "@Route doc-block annotations, route imports and prefixes in YAML (resource:), requirements and locale prefixes".into(),
                "services configured in services.yaml, aliases, tagged services and interfaces with several implementations".into(),
                "computed paths (reported, no relation)".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        declares(package, "symfony/framework-bundle")
            .or_else(|| declares(package, "symfony/symfony"))
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
            let Some(tree) = parse_php(&source) else {
                continue;
            };
            parsed.push((file, source, tree));
        }
        let index = class_index(&parsed);
        for (file, source, tree) in &parsed {
            let scan = ClassScan {
                package,
                file,
                source,
                index: &index,
                in_src: file
                    .as_path()
                    .starts_with(FrameworkInput::package_dir(package).join("src")),
            };
            for class in classes(tree.root_node(), source) {
                scan.class(&class, output);
            }
        }
        yaml_routes(
            package,
            input,
            &parsed
                .iter()
                .map(|(f, s, t)| (f.clone(), s.clone(), t.clone()))
                .collect::<Vec<_>>(),
            output,
        );
        Ok(())
    }
}

/// One PHP file being read for Symfony conventions.
struct ClassScan<'a> {
    package: &'a Package,
    file: &'a RootRelativePath,
    source: &'a str,
    index: &'a HashMap<String, Vec<RootRelativePath>>,
    in_src: bool,
}

impl ClassScan<'_> {
    fn unique(&self, name: &str) -> Option<RootRelativePath> {
        self.index
            .get(name)
            .filter(|f| f.len() == 1)
            .map(|f| f[0].clone())
    }

    fn relate(&self, draft: RelationDraft, output: &mut FrameworkOutput) {
        output.relate(FrameworkRelation::new(ID, Some(&self.package.id), draft));
    }

    fn class(&self, class: &crate::php_common::Class<'_>, output: &mut FrameworkOutput) {
        // A computed class-level route path means its routes were not read at all.
        if !self.routes(class, output) {
            return;
        }
        self.entity(class, output);
        self.console_command(class, output);
        self.autowiring(class, output);
    }

    /// `#[Route]` attributes on the class's methods, joined with the class-level prefix. False
    /// when the class-level path is computed.
    fn routes(&self, class: &crate::php_common::Class<'_>, output: &mut FrameworkOutput) -> bool {
        let source = self.source;
        let prefixes = match class.attribute("Route") {
            None => vec![String::new()],
            Some(route) => match route_path(route, source) {
                Some(path) => vec![path.unwrap_or_default()],
                None => {
                    output.unresolved(
                        self.file,
                        format!(
                            "`{}` has a computed #[Route] path; its routes were not read",
                            class.name
                        ),
                    );
                    return false;
                }
            },
        };
        for method in &class.methods {
            for route in method.attributes.iter().filter(|a| a.name == "Route") {
                let Some(path) = route_path(route, source) else {
                    output.unresolved(
                        self.file,
                        format!(
                            "`{}::{}` has a computed #[Route] path; no relation emitted",
                            class.name, method.name
                        ),
                    );
                    continue;
                };
                let path = path.unwrap_or_default();
                for prefix in &prefixes {
                    for verb in route_methods(route, source) {
                        let draft = RelationDraft {
                            kind: RelationKind::RouteRegisteredTo,
                            rule: ROUTE_RULE,
                            rule_version: VERSION,
                            certainty: Certainty::Declared,
                            from: self.file.clone(),
                            to: Some(self.file.clone()),
                            from_symbol: Some(class.name.clone()),
                            to_symbol: Some(format!("{}.{}", class.name, method.name)),
                            label: format!("{verb} {}", join(prefix, &path)),
                            evidence: evidence(self.file, route.node, source),
                        };
                        self.relate(draft, output);
                    }
                }
            }
        }
        true
    }

    /// A Doctrine `#[Entity]` maps to a table: the declared one, else the snake-cased name.
    fn entity(&self, class: &crate::php_common::Class<'_>, output: &mut FrameworkOutput) {
        let Some(entity) = class.attribute("Entity") else {
            return;
        };
        let table = class
            .attribute("Table")
            .and_then(|t| t.argument("name"))
            .and_then(|n| string_value(n, self.source));
        let (label, certainty) = match table {
            Some(name) => (name, Certainty::Declared),
            None => (snake_case(&class.name), Certainty::Convention),
        };
        let draft = RelationDraft {
            kind: RelationKind::ModelMappedToTable,
            rule: ENTITY_RULE,
            rule_version: VERSION,
            certainty,
            from: self.file.clone(),
            to: None,
            from_symbol: Some(class.name.clone()),
            to_symbol: None,
            label,
            evidence: evidence(self.file, entity.node, self.source),
        };
        self.relate(draft, output);
    }

    /// `#[AsCommand(name: …)]` exposes a console command.
    fn console_command(&self, class: &crate::php_common::Class<'_>, output: &mut FrameworkOutput) {
        let Some(command) = class.attribute("AsCommand") else {
            return;
        };
        let Some(name) = command
            .argument("name")
            .or_else(|| command.first())
            .and_then(|n| string_value(n, self.source))
        else {
            return;
        };
        let draft = RelationDraft {
            kind: RelationKind::CommandExposedAs,
            rule: COMMAND_RULE,
            rule_version: VERSION,
            certainty: Certainty::Declared,
            from: self.file.clone(),
            to: Some(self.file.clone()),
            from_symbol: None,
            to_symbol: Some(class.name.clone()),
            label: name,
            evidence: evidence(self.file, command.node, self.source),
        };
        self.relate(draft, output);
    }

    /// Constructor parameters of a service in `src/` are autowired from the class they name.
    fn autowiring(&self, class: &crate::php_common::Class<'_>, output: &mut FrameworkOutput) {
        if !self.in_src || class.attribute("Entity").is_some() {
            return;
        }
        let Some(constructor) = class.constructor() else {
            return;
        };
        for parameter in &constructor.parameters {
            let Some(provider) = self.unique(&parameter.type_name) else {
                continue;
            };
            if &provider == self.file {
                continue;
            }
            let draft = RelationDraft {
                kind: RelationKind::ProviderInjectedInto,
                rule: AUTOWIRE_RULE,
                rule_version: VERSION,
                certainty: Certainty::Convention,
                from: provider,
                to: Some(self.file.clone()),
                from_symbol: Some(parameter.type_name.clone()),
                to_symbol: Some(class.name.clone()),
                label: parameter.type_name.clone(),
                evidence: evidence(self.file, parameter.node, self.source),
            };
            self.relate(draft, output);
        }
    }
}

/// The path of a `#[Route]`: `Some(Some(path))` literal, `Some(None)` absent, `None` when computed.
fn route_path(route: &Attribute<'_>, source: &str) -> Option<Option<String>> {
    match route.argument("path").or_else(|| route.first()) {
        None => Some(None),
        Some(node) => string_value(node, source).map(Some),
    }
}

fn route_methods(route: &Attribute<'_>, source: &str) -> Vec<String> {
    let Some(list) = route.argument("methods") else {
        return vec!["ANY".into()];
    };
    let mut cursor = list.walk();
    let verbs: Vec<String> = list
        .named_children(&mut cursor)
        .filter_map(|e| e.named_child(0))
        .filter_map(|s| string_value(s, source))
        .map(|s| s.to_uppercase())
        .collect();
    if verbs.is_empty() {
        vec!["ANY".into()]
    } else {
        verbs
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

/// Entries of `config/routes.yaml` and `config/routes/*.yaml` that have a path and a controller.
fn yaml_routes(
    package: &Package,
    input: &FrameworkInput<'_>,
    parsed: &[(RootRelativePath, String, tree_sitter::Tree)],
    output: &mut FrameworkOutput,
) {
    let directory = FrameworkInput::package_dir(package).join("config");
    let mut files: Vec<RootRelativePath> = Vec::new();
    for extension in ["yaml", "yml"] {
        files.extend(input.list_files(&directory, extension));
    }
    files.sort();
    let classes_by_fqn = classes_by_fqn(parsed);
    for file in files {
        if !file.as_path().to_string_lossy().contains("config/routes") {
            continue;
        }
        let Some(source) = input.read(&file) else {
            continue;
        };
        for block in route_blocks(&source) {
            relate_yaml_route(package, &file, &source, &block, &classes_by_fqn, output);
        }
    }
}

/// Fully qualified class name to file, from `namespace` and class declarations.
fn classes_by_fqn(
    parsed: &[(RootRelativePath, String, tree_sitter::Tree)],
) -> HashMap<String, RootRelativePath> {
    let mut classes = HashMap::new();
    for (file, source, tree) in parsed {
        let mut namespace = String::new();
        walk(tree.root_node(), &mut |node: Node<'_>| match node.kind() {
            "namespace_definition" => {
                namespace = node
                    .child_by_field_name("name")
                    .map(|n| text(n, source).to_owned())
                    .unwrap_or_default();
            }
            "class_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    classes.insert(format!("{namespace}\\{}", text(name, source)), file.clone());
                }
            }
            _ => {}
        });
    }
    classes
}

/// A routes file as blocks: a key at column 0, then its indented lines, each with its offset.
fn route_blocks(source: &str) -> Vec<Vec<(usize, &str)>> {
    let mut offset = 0usize;
    let mut block: Vec<(usize, &str)> = Vec::new();
    let mut blocks: Vec<Vec<(usize, &str)>> = Vec::new();
    for line in source.split_inclusive('\n') {
        if !line.starts_with([' ', '\t'])
            && !line.trim().is_empty()
            && !line.starts_with('#')
            && !block.is_empty()
        {
            blocks.push(std::mem::take(&mut block));
        }
        block.push((offset, line.trim_end()));
        offset += line.len();
    }
    blocks.push(block);
    blocks
}

/// The value of `key:` in a block, with the offset of its line.
fn block_value(block: &[(usize, &str)], key: &str) -> Option<(usize, String)> {
    block.iter().find_map(|(at, line)| {
        line.trim_start()
            .strip_prefix(&format!("{key}:"))
            .map(|v| (*at, v.trim().trim_matches(['\'', '"']).to_owned()))
    })
}

fn relate_yaml_route(
    package: &Package,
    file: &RootRelativePath,
    source: &str,
    block: &[(usize, &str)],
    classes_by_fqn: &HashMap<String, RootRelativePath>,
    output: &mut FrameworkOutput,
) {
    let (Some((path_at, path)), Some((_, controller))) =
        (block_value(block, "path"), block_value(block, "controller"))
    else {
        return;
    };
    let (class, action) = controller
        .split_once("::")
        .map_or((controller.as_str(), None), |(c, a)| (c, Some(a)));
    let target = classes_by_fqn.get(class.trim_start_matches('\\')).cloned();
    let verbs: Vec<String> = block_value(block, "methods")
        .map(|(_, v)| {
            v.trim_matches(['[', ']'])
                .split(',')
                .map(|m| m.trim().trim_matches(['\'', '"']).to_uppercase())
                .filter(|m| !m.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let verbs = if verbs.is_empty() {
        vec!["ANY".to_owned()]
    } else {
        verbs
    };
    let simple = class.rsplit('\\').next().unwrap_or(class);
    let line_end = path_at
        + source[path_at..]
            .find('\n')
            .unwrap_or(source.len() - path_at);
    for verb in verbs {
        let draft = RelationDraft {
            kind: RelationKind::RouteRegisteredTo,
            rule: YAML_RULE,
            rule_version: VERSION,
            certainty: Certainty::Declared,
            from: file.clone(),
            to: target.clone(),
            from_symbol: None,
            to_symbol: Some(action.map_or(simple.to_owned(), |a| format!("{simple}.{a}"))),
            label: format!("{verb} {}", join("", &path)),
            evidence: RelationEvidence::new(
                file,
                ByteSpan {
                    start: path_at,
                    end: path_at + path.len(),
                },
                &source[path_at..line_end],
            ),
        };
        output.relate(FrameworkRelation::new(ID, Some(&package.id), draft));
    }
}
