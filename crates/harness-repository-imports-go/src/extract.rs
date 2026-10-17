//! Go import extraction: single and grouped `import` declarations, with aliases, blank and dot imports.
use harness_workspace::{
    import_graph::{
        ByteSpan, DeclaredItem, ExtractedFile, FactBuilder, ImportForm, ImportLimits, ItemKind,
        LanguageImportAdapter, LanguageProfile, Visibility,
    },
    repository_intelligence::RootRelativePath,
};
use tree_sitter::Node;

pub const ADAPTER_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default)]
pub struct GoImportAdapter;

impl LanguageImportAdapter for GoImportAdapter {
    fn id(&self) -> &'static str {
        "go-imports"
    }

    fn version(&self) -> u32 {
        ADAPTER_VERSION
    }

    fn profile_for(&self, path: &RootRelativePath) -> Option<LanguageProfile> {
        (path.as_path().extension()?.to_str()? == "go").then_some(LanguageProfile::Go)
    }

    fn forms(&self) -> Vec<ImportForm> {
        vec![ImportForm::Import]
    }

    fn extract(
        &self,
        path: &RootRelativePath,
        profile: LanguageProfile,
        source: &str,
        limits: &ImportLimits,
    ) -> ExtractedFile {
        let mut out = ExtractedFile::default();
        let mut parser = tree_sitter::Parser::new();
        if parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .is_err()
        {
            out.diagnostics.push("grammar is unavailable".into());
            return out;
        }
        let Some(tree) = parser.parse(source, None) else {
            out.diagnostics.push("parse produced no tree".into());
            return out;
        };
        if tree.root_node().has_error() {
            out.diagnostics
                .push("source has syntax errors; extraction may be incomplete".into());
        }
        let builder = FactBuilder { path, profile };
        let root = tree.root_node();
        let mut cursor = root.walk();
        for declaration in root.children(&mut cursor) {
            match declaration.kind() {
                "import_declaration" => {
                    let mut specs = Vec::new();
                    collect_specs(declaration, &mut specs);
                    for spec in specs {
                        if out.facts.len() >= limits.max_facts_per_file {
                            out.truncated = true;
                            break;
                        }
                        let Some(path_node) = spec.child_by_field_name("path") else {
                            continue;
                        };
                        let raw = &source[path_node.start_byte()..path_node.end_byte()];
                        let spelling = raw.trim_matches(['"', '`']).to_owned();
                        let mut fact = builder.fact(
                            ByteSpan {
                                start: spec.start_byte(),
                                end: spec.end_byte(),
                            },
                            "",
                            spelling.clone(),
                            ImportForm::Import,
                        );
                        if let Some(name) = spec.child_by_field_name("name") {
                            let name = &source[name.start_byte()..name.end_byte()];
                            match name {
                                "." => fact.glob = true,
                                alias => fact.alias = Some(alias.to_owned()),
                            }
                        }
                        if spelling == "C" {
                            out.diagnostics
                                .push("`import \"C\"` (cgo) is not followed".into());
                        }
                        out.facts.push(fact);
                    }
                }
                "function_declaration"
                | "method_declaration"
                | "type_declaration"
                | "var_declaration"
                | "const_declaration" => items(declaration, source, &mut out),
                _ => {}
            }
        }
        out
    }
}

fn collect_specs<'t>(node: Node<'t>, specs: &mut Vec<Node<'t>>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "import_spec" => specs.push(child),
            "import_spec_list" => collect_specs(child, specs),
            _ => {}
        }
    }
}

fn items(declaration: Node<'_>, source: &str, out: &mut ExtractedFile) {
    let mut push = |name: Node<'_>, kind: ItemKind| {
        let text = source[name.start_byte()..name.end_byte()].to_owned();
        let exported = text.starts_with(|c: char| c.is_uppercase());
        out.items.push(DeclaredItem {
            name: text,
            kind,
            visibility: if exported {
                Visibility::Public
            } else {
                Visibility::Private
            },
            span: ByteSpan {
                start: name.start_byte(),
                end: name.end_byte(),
            },
            scope: Vec::new(),
        });
    };
    match declaration.kind() {
        "function_declaration" => {
            if let Some(name) = declaration.child_by_field_name("name") {
                push(name, ItemKind::Function);
            }
        }
        "type_declaration" | "var_declaration" | "const_declaration" => {
            let mut cursor = declaration.walk();
            let specs: Vec<Node<'_>> = declaration.named_children(&mut cursor).collect();
            for spec in specs {
                let (kind, field) = match spec.kind() {
                    "type_spec" => (ItemKind::Struct, "name"),
                    "var_spec" => (ItemKind::Variable, "name"),
                    "const_spec" => (ItemKind::Const, "name"),
                    _ => continue,
                };
                if let Some(name) = spec.child_by_field_name(field) {
                    push(name, kind);
                }
            }
        }
        _ => {}
    }
}
