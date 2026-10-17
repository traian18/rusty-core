//! React: which component renders which, read from JSX in the files that define components.
use std::collections::HashSet;

use harness_workspace::{
    framework_relations::{
        declares, Certainty, FrameworkAdapter, FrameworkCapability, FrameworkInput,
        FrameworkOutput, FrameworkRelation, RelationDraft, RelationKind,
    },
    repository_intelligence::{DiscoveryError, Package},
};
use tokio_util::sync::CancellationToken;
use tree_sitter::Node;

use crate::{
    handlers::top_level_names,
    syntax::{evidence, is_script, parse_script, text, walk},
};

const ID: &str = "react";
const VERSION: u32 = 1;
const RULE: &str = "react-jsx-render";

pub struct ReactAdapter;

impl FrameworkAdapter for ReactAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "React".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::ComponentRenders],
            handled: vec![
                "JSX elements with a capitalised name (<Card />, <Layout.Header />) inside function, arrow, memo/forwardRef or class components, resolved to a component declared in the same file or imported from another file of the repository".into(),
                "JSX outside any component (for example the render call of the entry file) is attributed to the file".into(),
            ],
            unsupported: vec![
                "components from packages outside the repository (no file to point at)".into(),
                "React.lazy and other dynamic imports, components chosen at run time, render props and children passed as values".into(),
                "components re-exported through barrel files whose re-export the import index does not resolve".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        declares(package, "react")
    }

    fn extract(
        &self,
        package: &Package,
        input: &FrameworkInput<'_>,
        cancel: &CancellationToken,
        output: &mut FrameworkOutput,
    ) -> Result<(), DiscoveryError> {
        let files: Vec<_> = input
            .files_of(package)
            .into_iter()
            .filter(|f| is_script(&f.path))
            .map(|f| f.path.clone())
            .collect();
        if cancel.is_cancelled() {
            return Err(DiscoveryError::Cancelled);
        }
        for part in crate::parallel::map(&files, |file| read_file(package, file, input)) {
            output.relations.extend(part.relations);
            output.diagnostics.extend(part.diagnostics);
        }
        Ok(())
    }
}

/// The component relations of one file.
fn read_file(
    package: &Package,
    file: &harness_workspace::repository_intelligence::RootRelativePath,
    input: &FrameworkInput<'_>,
) -> FrameworkOutput {
    let mut output = FrameworkOutput::default();
    let Some(source) = input.read(file) else {
        return output;
    };
    if !source.contains('<') {
        return output;
    }
    let Some(tree) = parse_script(file, &source) else {
        return output;
    };
    let root = tree.root_node();
    let locals = top_level_names(root, &source);
    let components = components(root, &source);
    let mut seen: HashSet<(Option<String>, String)> = HashSet::new();
    walk(root, &mut |node| {
        if !matches!(
            node.kind(),
            "jsx_opening_element" | "jsx_self_closing_element"
        ) {
            return;
        }
        let Some(tag) = node
            .child_by_field_name("name")
            .and_then(|n| root_name(n, &source))
        else {
            return;
        };
        if !tag.starts_with(|c: char| c.is_ascii_uppercase()) {
            return;
        }
        let enclosing = components
            .iter()
            .filter(|(_, start, end)| *start <= node.start_byte() && node.end_byte() <= *end)
            .min_by_key(|(_, start, end)| end - start)
            .map(|(name, _, _)| name.clone());
        let target = if locals.contains(&tag) {
            Some(file.clone())
        } else {
            input.names.target(file, &tag).cloned()
        };
        let Some(target) = target else { return };
        if enclosing.as_deref() == Some(tag.as_str()) && &target == file {
            return;
        }
        if !seen.insert((enclosing.clone(), tag.clone())) {
            return;
        }
        output.relate(FrameworkRelation::new(
            ID,
            Some(&package.id),
            RelationDraft {
                kind: RelationKind::ComponentRenders,
                rule: RULE,
                rule_version: VERSION,
                certainty: Certainty::Declared,
                from: file.clone(),
                to: Some(target),
                from_symbol: enclosing,
                to_symbol: Some(tag.clone()),
                label: format!("<{tag} />"),
                evidence: evidence(file, node, &source),
            },
        ));
    });
    output
}

/// `Card` for `<Card>`, `Layout` for `<Layout.Header>`.
fn root_name(node: Node<'_>, source: &str) -> Option<String> {
    let mut current = node;
    while current.kind() == "member_expression" || current.kind() == "nested_identifier" {
        current = current
            .child_by_field_name("object")
            .or_else(|| current.named_child(0))?;
    }
    (current.kind() == "identifier").then(|| text(current, source).to_owned())
}

/// Components declared in the file with the byte range of their definition.
fn components(root: Node<'_>, source: &str) -> Vec<(String, usize, usize)> {
    let mut found = Vec::new();
    walk(root, &mut |node| {
        let (name, range) = match node.kind() {
            "function_declaration" | "class_declaration" => {
                (node.child_by_field_name("name"), node)
            }
            "variable_declarator" => {
                let value = node.child_by_field_name("value");
                let is_component_value = value.is_some_and(|v| {
                    matches!(
                        v.kind(),
                        "arrow_function" | "function_expression" | "function" | "call_expression"
                    )
                });
                (
                    is_component_value
                        .then(|| node.child_by_field_name("name"))
                        .flatten(),
                    node,
                )
            }
            _ => return,
        };
        let Some(name) = name.filter(|n| n.kind() == "identifier" || n.kind() == "type_identifier")
        else {
            return;
        };
        let name = text(name, source);
        if name.starts_with(|c: char| c.is_ascii_uppercase()) {
            found.push((name.to_owned(), range.start_byte(), range.end_byte()));
        }
    });
    found
}
