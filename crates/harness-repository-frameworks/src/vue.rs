//! Vue: components rendered by single-file components' templates, and Vue Router entries.
use std::collections::{HashMap, HashSet};

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
    angular::element_tags,
    routes::{route_table, Target},
    syntax::{
        evidence, is_script, join_relative, parse_script, parse_typescript, string_value, text,
        walk,
    },
};

const ID: &str = "vue";
const VERSION: u32 = 1;
const RENDER_RULE: &str = "vue-template-component";
const ROUTE_RULE: &str = "vue-router-entry";

pub struct VueAdapter;

impl FrameworkAdapter for VueAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "Vue".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::ComponentRenders, RelationKind::RouteRegisteredTo],
            handled: vec![
                "single-file components: a component imported from a .vue file (relative or @/ meaning src/) and used in the template as <PascalCase /> or <kebab-case />".into(),
                "Vue Router entries { path, component } with a component imported from a .vue or script file or loaded with () => import('./X.vue'), in files that use vue-router, with children paths joined".into(),
            ],
            unsupported: vec![
                "other import aliases (reported, no relation), components registered globally or by name strings, dynamic <component :is>".into(),
                "components defined inline in script without a .vue file".into(),
                "template syntax inside expressions; only element names are read".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        declares(package, "vue")
    }

    fn extract(
        &self,
        package: &Package,
        input: &FrameworkInput<'_>,
        cancel: &CancellationToken,
        output: &mut FrameworkOutput,
    ) -> Result<(), DiscoveryError> {
        let directory = FrameworkInput::package_dir(package);
        let sfc_files = input.list_files(&directory, "vue");
        let sfcs: HashSet<RootRelativePath> = sfc_files.iter().cloned().collect();
        let scripts: HashSet<RootRelativePath> =
            input.imports.files.iter().map(|f| f.path.clone()).collect();
        let source_root = directory.join("src");

        for file in &sfc_files {
            if cancel.is_cancelled() {
                return Err(DiscoveryError::Cancelled);
            }
            let Some(source) = input.read(file) else {
                continue;
            };
            let blocks = blocks(&source);
            let Some(template) = blocks.iter().find(|b| b.tag == "template") else {
                continue;
            };
            let mut bindings: HashMap<String, (RootRelativePath, bool)> = HashMap::new();
            for script in blocks.iter().filter(|b| b.tag == "script") {
                let Some(tree) = parse_typescript(&source[script.start..script.end]) else {
                    continue;
                };
                imports_of(
                    tree.root_node(),
                    &source[script.start..script.end],
                    file,
                    &sfcs,
                    &scripts,
                    &source_root,
                    output,
                    &mut bindings,
                );
            }
            let mut seen = HashSet::new();
            for (tag, offset) in element_tags(&source[template.start..template.end]) {
                let Some((binding, (target, convention))) = bindings
                    .iter()
                    .find(|(name, _)| tag == **name || tag == kebab(name))
                else {
                    continue;
                };
                if !seen.insert(binding.clone()) {
                    continue;
                }
                let start = template.start + offset;
                let end = source[start..]
                    .find('>')
                    .map_or(source.len(), |i| start + i + 1);
                output.relate(FrameworkRelation::new(
                    ID,
                    Some(&package.id),
                    RelationDraft {
                        kind: RelationKind::ComponentRenders,
                        rule: RENDER_RULE,
                        rule_version: VERSION,
                        certainty: if *convention {
                            Certainty::Convention
                        } else {
                            Certainty::Declared
                        },
                        from: file.clone(),
                        to: Some(target.clone()),
                        from_symbol: None,
                        to_symbol: Some(binding.clone()),
                        label: format!("<{tag} />"),
                        evidence: RelationEvidence::new(
                            file,
                            ByteSpan { start, end },
                            &source[start..end],
                        ),
                    },
                ));
            }
        }

        // Router entries in script files.
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
            if !source.contains("vue-router") {
                continue;
            }
            let Some(tree) = parse_script(&indexed.path, &source) else {
                continue;
            };
            let root = tree.root_node();
            let file = &indexed.path;
            let mut bindings = HashMap::new();
            imports_of(
                root,
                &source,
                file,
                &sfcs,
                &scripts,
                &source_root,
                output,
                &mut bindings,
            );
            let table = route_table(root, &source, &["component"]);
            for message in table.skipped {
                output.unresolved(file, message);
            }
            for route in table.routes {
                let (target, symbol, convention) = match &route.target {
                    Target::Name(name) => match bindings.get(name) {
                        Some((path, convention)) => {
                            (Some(path.clone()), Some(name.clone()), *convention)
                        }
                        None => (
                            input.names.target(file, name).cloned(),
                            Some(name.clone()),
                            false,
                        ),
                    },
                    Target::Lazy(specifier) => {
                        let (path, convention) =
                            resolve_specifier(specifier, file, &sfcs, &scripts, &source_root);
                        (path, None, convention)
                    }
                    Target::Other(node) => {
                        output.unresolved(
                            file,
                            format!(
                                "route `{}` names a component this reader does not follow (`{}`)",
                                route.path,
                                text(*node, &source)
                            ),
                        );
                        continue;
                    }
                };
                output.relate(FrameworkRelation::new(
                    ID,
                    Some(&package.id),
                    RelationDraft {
                        kind: RelationKind::RouteRegisteredTo,
                        rule: ROUTE_RULE,
                        rule_version: VERSION,
                        certainty: if convention {
                            Certainty::Convention
                        } else {
                            Certainty::Declared
                        },
                        from: file.clone(),
                        to: target,
                        from_symbol: None,
                        to_symbol: symbol,
                        label: format!("ROUTE {}", route.path),
                        evidence: evidence(file, route.node, &source),
                    },
                ));
            }
        }
        Ok(())
    }
}

struct Block {
    tag: &'static str,
    /// Byte range of the block's content within the file.
    start: usize,
    end: usize,
}

/// The top-level `<template>` and `<script>` blocks of a single-file component. Nested
/// `<template>` elements are part of the outer template.
fn blocks(source: &str) -> Vec<Block> {
    let mut found = Vec::new();
    let mut index = 0;
    while let Some(open) = source[index..].find('<').map(|i| index + i) {
        let rest = &source[open..];
        let name = if rest.starts_with("<template") {
            Some("template")
        } else if rest.starts_with("<script") {
            Some("script")
        } else if rest.starts_with("<style") {
            Some("style")
        } else {
            None
        };
        let Some(name) = name else {
            index = open + 1;
            continue;
        };
        let Some(content_start) = source[open..].find('>').map(|i| open + i + 1) else {
            break;
        };
        let close_tag = format!("</{name}>");
        let content_end = if name == "template" {
            // Match nested templates by counting opens and closes.
            let mut depth = 1usize;
            let mut cursor = content_start;
            let mut end = None;
            while depth > 0 {
                let next_open = source[cursor..].find("<template").map(|i| cursor + i);
                let next_close = source[cursor..].find("</template>").map(|i| cursor + i);
                match (next_open, next_close) {
                    (Some(o), Some(c)) if o < c => {
                        depth += 1;
                        cursor = o + "<template".len();
                    }
                    (_, Some(c)) => {
                        depth -= 1;
                        cursor = c + close_tag.len();
                        if depth == 0 {
                            end = Some(c);
                        }
                    }
                    _ => break,
                }
            }
            end
        } else {
            source[content_start..]
                .find(&close_tag)
                .map(|i| content_start + i)
        };
        let Some(content_end) = content_end else {
            break;
        };
        match name {
            "template" => found.push(Block {
                tag: "template",
                start: content_start,
                end: content_end,
            }),
            "script" => found.push(Block {
                tag: "script",
                start: content_start,
                end: content_end,
            }),
            _ => {}
        }
        index = content_end + close_tag.len();
    }
    found
}

/// `MyCard` becomes `my-card`.
fn kebab(name: &str) -> String {
    let mut out = String::new();
    for (position, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if position > 0 {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// The file a specifier names: a relative path, or `@/` meaning the package's `src`.
/// The flag is true for the `@/` alias, which is a convention rather than something in the code.
fn resolve_specifier(
    specifier: &str,
    file: &RootRelativePath,
    sfcs: &HashSet<RootRelativePath>,
    scripts: &HashSet<RootRelativePath>,
    source_root: &std::path::Path,
) -> (Option<RootRelativePath>, bool) {
    let (base, convention) = if let Some(rest) = specifier.strip_prefix("@/") {
        (RootRelativePath::new(source_root.join(rest)).ok(), true)
    } else {
        (join_relative(file, specifier), false)
    };
    let resolved = base.and_then(|base| {
        if sfcs.contains(&base) {
            return Some(base);
        }
        crate::syntax::find_module(&base, |c| scripts.contains(c), &["ts", "tsx", "js", "jsx"])
    });
    (resolved, convention)
}

/// Collects `local name -> file` for imports that resolve to a component file, and reports
/// `.vue` imports through an alias this reader does not know.
#[allow(clippy::too_many_arguments)]
fn imports_of(
    root: Node<'_>,
    source: &str,
    file: &RootRelativePath,
    sfcs: &HashSet<RootRelativePath>,
    scripts: &HashSet<RootRelativePath>,
    source_root: &std::path::Path,
    output: &mut FrameworkOutput,
    bindings: &mut HashMap<String, (RootRelativePath, bool)>,
) {
    walk(root, &mut |node| {
        if node.kind() != "import_statement" {
            return;
        }
        let Some(specifier) = node
            .child_by_field_name("source")
            .and_then(|s| string_value(s, source))
        else {
            return;
        };
        let (resolved, convention) =
            resolve_specifier(&specifier, file, sfcs, scripts, source_root);
        let Some(resolved) = resolved else {
            if specifier.ends_with(".vue") {
                output.unresolved(file, format!("import of `{specifier}` could not be resolved (unknown alias or missing file); no relation emitted"));
            }
            return;
        };
        let mut names = Vec::new();
        walk(node, &mut |inner| match inner.kind() {
            "import_clause" => {
                if let Some(first) = inner.named_child(0).filter(|c| c.kind() == "identifier") {
                    names.push(text(first, source).to_owned());
                }
            }
            "import_specifier" => {
                let local = inner
                    .child_by_field_name("alias")
                    .or_else(|| inner.child_by_field_name("name"));
                if let Some(local) = local {
                    names.push(text(local, source).to_owned());
                }
            }
            "namespace_import" => {
                if let Some(name) = inner.named_child(0) {
                    names.push(text(name, source).to_owned());
                }
            }
            _ => {}
        });
        for name in names {
            bindings
                .entry(name)
                .or_insert((resolved.clone(), convention));
        }
    });
}
