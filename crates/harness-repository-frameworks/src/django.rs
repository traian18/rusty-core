//! Django: URL patterns in `urls.py`, models mapped to tables, and management commands.
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
    py_common::{local_names, python_files, resolve_view},
    py_items::{call_arguments, join_path, string_value},
    syntax::{evidence, parse_python, text, walk},
};

const ID: &str = "django";
const VERSION: u32 = 1;
const URL_RULE: &str = "django-urlpatterns";
const MODEL_RULE: &str = "django-model-table";
const COMMAND_RULE: &str = "django-management-command";

pub struct DjangoAdapter;

impl FrameworkAdapter for DjangoAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "Django".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::RouteRegisteredTo, RelationKind::ModelMappedToTable, RelationKind::CommandExposedAs],
            handled: vec![
                "path(), re_path() and url() entries with a literal pattern in urls.py, naming a view function, module.function or Class.as_view(); the method is not part of a Django route, so the label starts with ANY".into(),
                "classes deriving from models.Model or Model: the table is db_table when it is a literal in class Meta (declared), otherwise <app>_<lowercased class> by Django's naming rule (convention); abstract models are skipped".into(),
                "management/commands/<name>.py files with a class Command".into(),
            ],
            unsupported: vec![
                "include() groupings: their prefixes are not joined and the included urlconf is not followed".into(),
                "patterns built from computed strings, class-based view mixins, DRF routers and viewsets".into(),
                "models with a custom app_label or swapped table names set outside the class".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        declares(package, "django")
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
            let name = file
                .as_path()
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            let in_commands = file
                .as_path()
                .components()
                .any(|c| c.as_os_str() == "commands")
                && file
                    .as_path()
                    .components()
                    .any(|c| c.as_os_str() == "management");
            let Some(source) = input.read(&file) else {
                continue;
            };
            let wants_urls = name == "urls.py" || source.contains("urlpatterns");
            let wants_models = source.contains("models.Model") || source.contains("(Model)");
            if !wants_urls && !wants_models && !in_commands {
                continue;
            }
            let Some(tree) = parse_python(&source) else {
                continue;
            };
            let root = tree.root_node();
            if wants_urls {
                urls(package, &file, root, &source, input, output);
            }
            if wants_models {
                models(package, &file, root, &source, output);
            }
            if in_commands && name != "__init__.py" {
                command(package, &file, root, &source, output);
            }
        }
        Ok(())
    }
}

fn urls(
    package: &Package,
    file: &RootRelativePath,
    root: Node<'_>,
    source: &str,
    input: &FrameworkInput<'_>,
    output: &mut FrameworkOutput,
) {
    let locals = local_names(root, source);
    walk(root, &mut |node| {
        if node.kind() != "call" {
            return;
        }
        let Some(function) = node
            .child_by_field_name("function")
            .filter(|f| f.kind() == "identifier")
        else {
            return;
        };
        let callee = text(function, source);
        if !matches!(callee, "path" | "re_path" | "url") {
            return;
        }
        // Only entries of a `urlpatterns` list count; other calls named `path` are not routes.
        let mut in_patterns = false;
        let mut current = node.parent();
        while let Some(ancestor) = current {
            if ancestor.kind() == "assignment"
                && ancestor
                    .child_by_field_name("left")
                    .is_some_and(|l| text(l, source) == "urlpatterns")
            {
                in_patterns = true;
                break;
            }
            if ancestor.kind() == "augmented_assignment"
                && ancestor
                    .child_by_field_name("left")
                    .is_some_and(|l| text(l, source) == "urlpatterns")
            {
                in_patterns = true;
                break;
            }
            current = ancestor.parent();
        }
        if !in_patterns {
            return;
        }
        let (args, _) = call_arguments(node, source);
        let (Some(pattern), Some(view)) = (args.first(), args.get(1)) else {
            return;
        };
        if view.kind() == "call"
            && view
                .child_by_field_name("function")
                .is_some_and(|f| text(f, source) == "include")
        {
            return; // a grouping: documented as not followed
        }
        let Some(pattern) = string_value(*pattern, source) else {
            output.unresolved(
                file,
                format!(
                    "`{callee}({}, …)` has a computed pattern; no relation emitted",
                    text(*pattern, source)
                ),
            );
            return;
        };
        let pattern = pattern
            .trim_start_matches('^')
            .trim_end_matches('$')
            .to_owned();
        let (target, symbol) = resolve_view(*view, file, source, &locals, input);
        output.relate(FrameworkRelation::new(
            ID,
            Some(&package.id),
            RelationDraft {
                kind: RelationKind::RouteRegisteredTo,
                rule: URL_RULE,
                rule_version: VERSION,
                certainty: Certainty::Declared,
                from: file.clone(),
                to: target,
                from_symbol: None,
                to_symbol: symbol,
                label: format!("ANY {}", join_path("", &pattern)),
                evidence: evidence(file, node, source),
            },
        ));
    });
}

fn app_label(file: &RootRelativePath) -> String {
    let mut directories = file
        .directory()
        .components()
        .rev()
        .map(|c| c.as_os_str().to_string_lossy().into_owned());
    match directories.next() {
        Some(first) if first == "models" => directories.next().unwrap_or(first),
        Some(first) => first,
        None => String::new(),
    }
}

fn models(
    package: &Package,
    file: &RootRelativePath,
    root: Node<'_>,
    source: &str,
    output: &mut FrameworkOutput,
) {
    walk(root, &mut |node| {
        if node.kind() != "class_definition" {
            return;
        }
        let Some(class_name) = node
            .child_by_field_name("name")
            .map(|n| text(n, source).to_owned())
        else {
            return;
        };
        let bases = node
            .child_by_field_name("superclasses")
            .map(|b| text(b, source).replace(' ', ""))
            .unwrap_or_default();
        if !(bases.contains("models.Model")
            || bases.contains("(Model)")
            || bases.contains("(Model,"))
        {
            return;
        }
        let mut db_table = None;
        let mut abstract_model = false;
        if let Some(body) = node.child_by_field_name("body") {
            let mut cursor = body.walk();
            for member in body.named_children(&mut cursor) {
                if member.kind() != "class_definition"
                    || member.child_by_field_name("name").map(|n| text(n, source)) != Some("Meta")
                {
                    continue;
                }
                walk(member, &mut |n| {
                    if n.kind() != "assignment" {
                        return;
                    }
                    let (Some(left), Some(right)) = (
                        n.child_by_field_name("left"),
                        n.child_by_field_name("right"),
                    ) else {
                        return;
                    };
                    match text(left, source) {
                        "db_table" => db_table = string_value(right, source),
                        "abstract" => abstract_model = text(right, source) == "True",
                        _ => {}
                    }
                });
            }
        }
        if abstract_model {
            return;
        }
        let (table, certainty) = match db_table {
            Some(table) => (table, Certainty::Declared),
            None => (
                format!("{}_{}", app_label(file), class_name.to_lowercase()),
                Certainty::Convention,
            ),
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
                from_symbol: Some(class_name),
                to_symbol: None,
                label: table,
                evidence: evidence(
                    file,
                    node.child_by_field_name("name").unwrap_or(node),
                    source,
                ),
            },
        ));
    });
}

fn command(
    package: &Package,
    file: &RootRelativePath,
    root: Node<'_>,
    source: &str,
    output: &mut FrameworkOutput,
) {
    let mut found = None;
    walk(root, &mut |node| {
        if node.kind() == "class_definition"
            && node.child_by_field_name("name").map(|n| text(n, source)) == Some("Command")
        {
            found.get_or_insert(node);
        }
    });
    let Some(class) = found else { return };
    let stem = file
        .as_path()
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_owned();
    output.relate(FrameworkRelation::new(
        ID,
        Some(&package.id),
        RelationDraft {
            kind: RelationKind::CommandExposedAs,
            rule: COMMAND_RULE,
            rule_version: VERSION,
            certainty: Certainty::Convention,
            from: file.clone(),
            to: Some(file.clone()),
            from_symbol: None,
            to_symbol: Some("Command".into()),
            label: stem,
            evidence: evidence(
                file,
                class.child_by_field_name("name").unwrap_or(class),
                source,
            ),
        },
    ));
}
