//! Tauri: functions exposed to the frontend with `#[tauri::command]` and registered in
//! `tauri::generate_handler![…]`.
use std::collections::BTreeMap;

use harness_workspace::{
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
    rust_items::functions,
    syntax::{parse_rust, text, walk},
};

const ID: &str = "tauri";
const VERSION: u32 = 1;
const RULE: &str = "tauri-generate-handler";

pub struct TauriAdapter;

impl FrameworkAdapter for TauriAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "Tauri".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::CommandExposedAs],
            handled: vec![
                "#[tauri::command] (or #[command] with use tauri::command) functions listed in tauri::generate_handler![…], including module::function entries; attributes on entries (#[cfg(…)]) are skipped over".into(),
                "an entry is resolved to the file that defines the command through imports and module declarations; when only the name is known and exactly one command of that name exists in the package it is used, marked as a convention".into(),
            ],
            unsupported: vec![
                "commands registered through custom macros or builders other than generate_handler!".into(),
                "entries whose module path goes deeper than module::function and cannot be resolved by name".into(),
                "commands defined but never registered are not reported".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        declares(package, "tauri")
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
            if indexed.path.as_path().extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let Some(source) = input.read(&indexed.path) else {
                continue;
            };
            if !source.contains("command") && !source.contains("generate_handler") {
                continue;
            }
            let Some(tree) = parse_rust(&source) else {
                continue;
            };
            parsed.push(Parsed {
                file: indexed.path.clone(),
                source,
                tree,
            });
        }

        // Where each command function is defined.
        let mut commands: BTreeMap<String, Vec<RootRelativePath>> = BTreeMap::new();
        for p in &parsed {
            for function in functions(p.tree.root_node(), &p.source) {
                if function.attribute(&["tauri::command", "command"]).is_some() {
                    commands
                        .entry(function.name.clone())
                        .or_default()
                        .push(p.file.clone());
                }
            }
        }
        let defines = |file: &RootRelativePath, name: &str| {
            commands.get(name).is_some_and(|files| files.contains(file))
        };

        for p in &parsed {
            let root = p.tree.root_node();
            walk(root, &mut |node| {
                if node.kind() != "macro_invocation" {
                    return;
                }
                let Some(name) = node.child_by_field_name("macro") else {
                    return;
                };
                if text(name, &p.source).rsplit("::").next() != Some("generate_handler") {
                    return;
                }
                let Some(tokens) = node
                    .named_children(&mut node.walk())
                    .find(|n| n.kind() == "token_tree")
                else {
                    return;
                };
                for entry in entries(tokens, &p.source) {
                    let command = entry.path.last().cloned().unwrap_or_default();
                    let mut certainty = Certainty::Declared;
                    let target = if entry.path.len() == 1 && defines(&p.file, &command) {
                        Some(p.file.clone())
                    } else if entry.path.len() == 1 {
                        input
                            .names
                            .target(&p.file, &command)
                            .filter(|f| defines(f, &command))
                            .cloned()
                    } else {
                        input
                            .names
                            .target(&p.file, &entry.path[0])
                            .filter(|f| defines(f, &command))
                            .cloned()
                    };
                    let target =
                        target.or_else(|| match commands.get(&command).map(Vec::as_slice) {
                            Some([only]) => {
                                certainty = Certainty::Convention;
                                Some(only.clone())
                            }
                            _ => None,
                        });
                    let Some(target) = target else {
                        output.unresolved(&p.file, format!("`{}` is listed in generate_handler! but no matching #[tauri::command] function was found", entry.path.join("::")));
                        continue;
                    };
                    output.relate(FrameworkRelation::new(
                        ID,
                        Some(&package.id),
                        RelationDraft {
                            kind: RelationKind::CommandExposedAs,
                            rule: RULE,
                            rule_version: VERSION,
                            certainty,
                            from: p.file.clone(),
                            to: Some(target),
                            from_symbol: None,
                            to_symbol: Some(command.clone()),
                            label: command,
                            evidence: RelationEvidence::new(
                                &p.file,
                                ByteSpan {
                                    start: entry.start,
                                    end: entry.end,
                                },
                                &p.source[entry.start..entry.end],
                            ),
                        },
                    ));
                }
            });
        }
        Ok(())
    }
}

struct Entry {
    path: Vec<String>,
    start: usize,
    end: usize,
}

/// The comma-separated paths in a `generate_handler!` token tree, without their attributes.
fn entries(tokens: Node<'_>, source: &str) -> Vec<Entry> {
    let mut found = Vec::new();
    let mut current: Option<Entry> = None;
    let mut cursor = tokens.walk();
    let children: Vec<Node<'_>> = tokens.children(&mut cursor).collect();
    let mut index = 0;
    while index < children.len() {
        let child = children[index];
        index += 1;
        match child.kind() {
            "(" | ")" | "[" | "]" | "{" | "}" => {}
            "," => {
                found.extend(current.take());
            }
            "#" => {
                // An attribute on the entry: `#` followed by a bracketed token tree.
                if children
                    .get(index)
                    .is_some_and(|n| n.kind() == "token_tree")
                {
                    index += 1;
                }
            }
            "identifier" => {
                let name = text(child, source).to_owned();
                match &mut current {
                    Some(entry) => {
                        entry.path.push(name);
                        entry.end = child.end_byte();
                    }
                    None => {
                        current = Some(Entry {
                            path: vec![name],
                            start: child.start_byte(),
                            end: child.end_byte(),
                        })
                    }
                }
            }
            _ => {}
        }
    }
    found.extend(current.take());
    found
}
