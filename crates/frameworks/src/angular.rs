//! Angular: components rendered by other components' templates, routes naming components, and
//! provider injection through constructors and `inject()`.
use std::collections::{BTreeMap, HashSet};

use rusty_repo_model::{
    framework_relations::{
        declares, Certainty, FrameworkAdapter, FrameworkCapability, FrameworkInput,
        FrameworkOutput, FrameworkRelation, RelationDraft, RelationEvidence, RelationKind,
    },
    import_graph::ByteSpan,
    repository_intelligence::{DiscoveryError, Package, RootRelativePath},
};
use tokio_util::sync::CancellationToken;

use crate::{
    decorators::{classes, injections, InjectionRules},
    handlers::top_level_names,
    routes::{pair_value, route_table, Target},
    syntax::{
        evidence, find_module, is_script, join_relative, parse_script, string_value, text, walk,
    },
};

const ID: &str = "angular";
const VERSION: u32 = 1;
const RENDER_RULE: &str = "angular-template-selector";
const ROUTE_RULE: &str = "angular-router-entry";
const INJECTION: InjectionRules = InjectionRules {
    framework: ID,
    rule: "angular-injection",
    version: VERSION,
    injectable: "Injectable",
    consumers: &["Component", "Directive", "Injectable", "Pipe", "NgModule"],
    inject_function: Some("inject"),
};
const SCRIPT_EXTENSIONS: [&str; 4] = ["ts", "tsx", "js", "jsx"];

pub struct AngularAdapter;

impl FrameworkAdapter for AngularAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "Angular".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::ComponentRenders, RelationKind::RouteRegisteredTo, RelationKind::ProviderInjectedInto],
            handled: vec![
                "@Component with a literal element selector, an inline template or a literal templateUrl; a component whose template uses another component's selector as an element".into(),
                "route entries { path, component } and { path, loadComponent: () => import('./x') } in files that use @angular/router, with children paths joined".into(),
                "constructor and inject(Type) injection of @Injectable classes defined in the repository".into(),
            ],
            unsupported: vec![
                "attribute, class and compound selectors, and elements inside structural directives that are built dynamically".into(),
                "loadChildren lazy modules and route data (reported, no relation)".into(),
                "providers configured in @NgModule or provider arrays, useClass/useFactory/useValue, injection tokens".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        declares(package, "@angular/core")
    }

    fn extract(
        &self,
        package: &Package,
        input: &FrameworkInput<'_>,
        cancel: &CancellationToken,
        output: &mut FrameworkOutput,
    ) -> Result<(), DiscoveryError> {
        let parsed = parse_files(package, input, cancel)?;
        let components = components_of(&parsed, input);
        let by_selector = components_by_selector(&components, output);
        relate_renders(package, &components, &by_selector, output);
        relate_routes(package, input, &parsed, output);
        injections(package, input, &INJECTION, output);
        Ok(())
    }
}

fn is_element_selector(selector: &str) -> bool {
    !selector.is_empty()
        && selector
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && selector.starts_with(|c: char| c.is_ascii_lowercase())
}

/// Element names opened in an HTML template with the byte offset of each `<`.
pub(crate) fn element_tags(template: &str) -> Vec<(String, usize)> {
    let bytes = template.as_bytes();
    let mut tags = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'<'
            && bytes
                .get(index + 1)
                .is_some_and(|b| b.is_ascii_alphabetic())
        {
            let start = index + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'-') {
                end += 1;
            }
            tags.push((template[start..end].to_owned(), index));
            index = end;
        } else {
            index += 1;
        }
    }
    tags
}

/// A script file that mentions Angular components or the router, parsed.
struct Parsed {
    file: RootRelativePath,
    source: String,
    tree: tree_sitter::Tree,
}

fn parse_files(
    package: &Package,
    input: &FrameworkInput<'_>,
    cancel: &CancellationToken,
) -> Result<Vec<Parsed>, DiscoveryError> {
    let mut parsed = Vec::new();
    for indexed in input.files_of(package) {
        if cancel.is_cancelled() {
            return Err(DiscoveryError::Cancelled);
        }
        if !is_script(&indexed.path) {
            continue;
        }
        let Some(source) = input.read(&indexed.path) else {
            continue;
        };
        if !(source.contains("@Component") || source.contains("@angular/router")) {
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
    Ok(parsed)
}

/// A `@Component` class: its selector, its template and where they live.
struct Component {
    file: RootRelativePath,
    class: String,
    selector: Option<String>,
    template: Option<(RootRelativePath, usize, String)>,
}

fn components_of(parsed: &[Parsed], input: &FrameworkInput<'_>) -> Vec<Component> {
    let mut components = Vec::new();
    for p in parsed {
        for class in classes(p.tree.root_node(), &p.source) {
            let Some(decorator) = class.decorator("Component") else {
                continue;
            };
            let Some(object) = decorator
                .arguments
                .and_then(|a| a.named_child(0))
                .filter(|n| n.kind() == "object")
            else {
                continue;
            };
            let selector =
                pair_value(object, "selector", &p.source).and_then(|n| string_value(n, &p.source));
            components.push(Component {
                file: p.file.clone(),
                class: class.name.clone(),
                selector,
                template: template_of(object, p, input),
            });
        }
    }
    components
}

/// The inline `template`, or the file `templateUrl` names, as (file, offset in file, body).
fn template_of(
    object: tree_sitter::Node<'_>,
    p: &Parsed,
    input: &FrameworkInput<'_>,
) -> Option<(RootRelativePath, usize, String)> {
    if let Some(node) = pair_value(object, "template", &p.source) {
        return string_value(node, &p.source)
            .map(|body| (p.file.clone(), node.start_byte() + 1, body));
    }
    let url =
        pair_value(object, "templateUrl", &p.source).and_then(|n| string_value(n, &p.source))?;
    let relative = if url.starts_with('.') {
        url
    } else {
        format!("./{url}")
    };
    join_relative(&p.file, &relative).and_then(|path| input.read(&path).map(|body| (path, 0, body)))
}

/// Components by element selector; a selector shared by several is reported and not used.
fn components_by_selector<'a>(
    components: &'a [Component],
    output: &mut FrameworkOutput,
) -> BTreeMap<&'a str, Vec<&'a Component>> {
    let mut by_selector: BTreeMap<&str, Vec<&Component>> = BTreeMap::new();
    for component in components {
        if let Some(selector) = component
            .selector
            .as_deref()
            .filter(|s| is_element_selector(s))
        {
            by_selector.entry(selector).or_default().push(component);
        }
    }
    for (selector, owners) in &by_selector {
        if owners.len() > 1 {
            output.unresolved(
                &owners[0].file,
                format!(
                    "selector `{selector}` is declared by {} components; its uses were not read",
                    owners.len()
                ),
            );
        }
    }
    by_selector
}

/// A component renders another when its template uses the other's element selector.
fn relate_renders(
    package: &Package,
    components: &[Component],
    by_selector: &BTreeMap<&str, Vec<&Component>>,
    output: &mut FrameworkOutput,
) {
    for parent in components {
        let Some((template_file, base, body)) = &parent.template else {
            continue;
        };
        let mut seen = HashSet::new();
        for (tag, offset) in element_tags(body) {
            let Some(owners) = by_selector.get(tag.as_str()) else {
                continue;
            };
            let [child] = owners.as_slice() else { continue };
            if child.file == parent.file && child.class == parent.class {
                continue;
            }
            if !seen.insert(tag.clone()) {
                continue;
            }
            let end = body[offset..]
                .find('>')
                .map_or(body.len(), |i| offset + i + 1);
            let draft = RelationDraft {
                kind: RelationKind::ComponentRenders,
                rule: RENDER_RULE,
                rule_version: VERSION,
                certainty: Certainty::Declared,
                from: parent.file.clone(),
                to: Some(child.file.clone()),
                from_symbol: Some(parent.class.clone()),
                to_symbol: Some(child.class.clone()),
                label: format!("<{tag}>"),
                evidence: RelationEvidence::new(
                    template_file,
                    ByteSpan {
                        start: base + offset,
                        end: base + end,
                    },
                    &body[offset..end],
                ),
            };
            output.relate(FrameworkRelation::new(ID, Some(&package.id), draft));
        }
    }
}

/// Router entries that name a component, in every file that uses the Angular router.
fn relate_routes(
    package: &Package,
    input: &FrameworkInput<'_>,
    parsed: &[Parsed],
    output: &mut FrameworkOutput,
) {
    let known: HashSet<RootRelativePath> =
        input.imports.files.iter().map(|f| f.path.clone()).collect();
    for p in parsed
        .iter()
        .filter(|p| p.source.contains("@angular/router"))
    {
        let root = p.tree.root_node();
        let table = route_table(root, &p.source, &["component", "loadComponent"]);
        for message in table.skipped {
            output.unresolved(&p.file, message);
        }
        report_lazy_children(root, p, output);
        let locals = top_level_names(root, &p.source);
        for route in table.routes {
            let (target_file, symbol) = match &route.target {
                Target::Name(name) => {
                    let file = if locals.contains(name) {
                        Some(p.file.clone())
                    } else {
                        input.names.target(&p.file, name).cloned()
                    };
                    (file, Some(name.clone()))
                }
                Target::Lazy(specifier) => {
                    let file = join_relative(&p.file, specifier).and_then(|base| {
                        find_module(&base, |c| known.contains(c), &SCRIPT_EXTENSIONS)
                    });
                    (file, None)
                }
                Target::Other(node) => {
                    output.unresolved(
                        &p.file,
                        format!(
                            "route `{}` names a component this reader does not follow (`{}`)",
                            route.path,
                            text(*node, &p.source)
                        ),
                    );
                    continue;
                }
            };
            let draft = RelationDraft {
                kind: RelationKind::RouteRegisteredTo,
                rule: ROUTE_RULE,
                rule_version: VERSION,
                certainty: Certainty::Declared,
                from: p.file.clone(),
                to: target_file,
                from_symbol: None,
                to_symbol: symbol,
                label: format!("ROUTE {}", route.path),
                evidence: evidence(&p.file, route.node, &p.source),
            };
            output.relate(FrameworkRelation::new(ID, Some(&package.id), draft));
        }
    }
}

fn report_lazy_children(root: tree_sitter::Node<'_>, p: &Parsed, output: &mut FrameworkOutput) {
    walk(root, &mut |node| {
        if node.kind() == "pair"
            && node
                .child_by_field_name("key")
                .is_some_and(|k| text(k, &p.source) == "loadChildren")
        {
            output.unresolved(
                &p.file,
                format!(
                    "`{}` lazy-loads children; those routes were not followed",
                    text(node, &p.source)
                ),
            );
        }
    });
}
