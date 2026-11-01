//! Tauri: functions exposed to the frontend with `#[tauri::command]` and registered in
//! `tauri::generate_handler![…]`.
use std::collections::BTreeMap;

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
        let parsed = parse_files(package, input, cancel)?;
        let commands = command_definitions(&parsed);
        for p in &parsed {
            let scan = FileScan {
                package,
                input,
                parsed: p,
                commands: &commands,
            };
            walk(p.tree.root_node(), &mut |node| {
                scan.handler_macro(node, output)
            });
        }
        Ok(())
    }
}

/// A Rust file that mentions Tauri commands, parsed.
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
    Ok(parsed)
}

type Commands = BTreeMap<String, Vec<RootRelativePath>>;

/// Where each `#[tauri::command]` function is defined.
fn command_definitions(parsed: &[Parsed]) -> Commands {
    let mut commands = Commands::new();
    for p in parsed {
        for function in functions(p.tree.root_node(), &p.source) {
            if function.attribute(&["tauri::command", "command"]).is_some() {
                commands
                    .entry(function.name.clone())
                    .or_default()
                    .push(p.file.clone());
            }
        }
    }
    commands
}

/// One file being read for `generate_handler!` lists.
struct FileScan<'a> {
    package: &'a Package,
    input: &'a FrameworkInput<'a>,
    parsed: &'a Parsed,
    commands: &'a Commands,
}

impl FileScan<'_> {
    fn defines(&self, file: &RootRelativePath, name: &str) -> bool {
        self.commands
            .get(name)
            .is_some_and(|files| files.contains(file))
    }

    /// Relates each command a `generate_handler![...]` lists to the function that defines it.
    fn handler_macro(&self, node: Node<'_>, output: &mut FrameworkOutput) {
        let p = self.parsed;
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
            self.relate_entry(&entry, output);
        }
    }

    fn relate_entry(&self, entry: &Entry, output: &mut FrameworkOutput) {
        let p = self.parsed;
        let command = entry.path.last().cloned().unwrap_or_default();
        let mut certainty = Certainty::Declared;
        let target = self.defining_file(entry, &command).or_else(|| {
            match self.commands.get(&command).map(Vec::as_slice) {
                Some([only]) => {
                    certainty = Certainty::Convention;
                    Some(only.clone())
                }
                _ => None,
            }
        });
        let Some(target) = target else {
            output.unresolved(&p.file, format!("`{}` is listed in generate_handler! but no matching #[tauri::command] function was found", entry.path.join("::")));
            return;
        };
        let draft = RelationDraft {
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
        };
        output.relate(FrameworkRelation::new(ID, Some(&self.package.id), draft));
    }

    /// The file that defines the listed command, by its path as written.
    fn defining_file(&self, entry: &Entry, command: &str) -> Option<RootRelativePath> {
        let file = &self.parsed.file;
        if entry.path.len() == 1 && self.defines(file, command) {
            return Some(file.clone());
        }
        let name = if entry.path.len() == 1 {
            command
        } else {
            &entry.path[0]
        };
        self.input
            .names
            .target(file, name)
            .filter(|f| self.defines(f, command))
            .cloned()
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
