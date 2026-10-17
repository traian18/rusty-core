//! Java import extraction: `import`, `import static` and wildcard imports, plus the file's
//! package and top-level types so imports can be matched to the files that define them.
use harness_workspace::{
    import_graph::{
        ByteSpan, DeclaredItem, ExtractedFile, FactBuilder, ImportForm, ImportLimits, ItemKind,
        LanguageImportAdapter, LanguageProfile, Visibility,
    },
    repository_intelligence::RootRelativePath,
};
use tree_sitter::Node;

pub const ADAPTER_VERSION: u32 = 1;

/// The item kind that stands for the file's `package` declaration (its name is the dotted package).
pub const PACKAGE_ITEM: ItemKind = ItemKind::Module;

#[derive(Clone, Copy, Debug, Default)]
pub struct JavaImportAdapter;

impl LanguageImportAdapter for JavaImportAdapter {
    fn id(&self) -> &'static str {
        "java-imports"
    }

    fn version(&self) -> u32 {
        ADAPTER_VERSION
    }

    fn profile_for(&self, path: &RootRelativePath) -> Option<LanguageProfile> {
        (path.as_path().extension()?.to_str()? == "java").then_some(LanguageProfile::Java)
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
            .set_language(&tree_sitter_java::LANGUAGE.into())
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
        for child in root.children(&mut cursor) {
            match child.kind() {
                "package_declaration" => {
                    if let Some(name) = child.named_child(0) {
                        out.items.push(item(
                            name,
                            text(name, source),
                            PACKAGE_ITEM,
                            Visibility::Public,
                        ));
                    }
                }
                "import_declaration" => {
                    if out.facts.len() >= limits.max_facts_per_file {
                        out.truncated = true;
                        continue;
                    }
                    import(child, source, &builder, &mut out);
                }
                "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
                | "annotation_type_declaration" => {
                    let kind = match child.kind() {
                        "interface_declaration" | "annotation_type_declaration" => {
                            ItemKind::Interface
                        }
                        "enum_declaration" => ItemKind::Enum,
                        "record_declaration" => ItemKind::Struct,
                        _ => ItemKind::Class,
                    };
                    let public = source[child.start_byte()..child.end_byte()]
                        .split('{')
                        .next()
                        .is_some_and(|head| head.contains("public "));
                    if let Some(name) = child.child_by_field_name("name") {
                        out.items.push(item(
                            name,
                            text(name, source),
                            kind,
                            if public {
                                Visibility::Public
                            } else {
                                Visibility::Crate
                            },
                        ));
                    }
                }
                _ => {}
            }
        }
        out
    }
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

fn item(node: Node<'_>, name: &str, kind: ItemKind, visibility: Visibility) -> DeclaredItem {
    DeclaredItem {
        name: name.to_owned(),
        kind,
        visibility,
        span: ByteSpan {
            start: node.start_byte(),
            end: node.end_byte(),
        },
        scope: Vec::new(),
    }
}

fn import(node: Node<'_>, source: &str, builder: &FactBuilder<'_>, out: &mut ExtractedFile) {
    let Some(name) = node.named_child(0) else {
        return;
    };
    let full = text(name, source).to_owned();
    let declaration = text(node, source);
    let is_static = declaration
        .trim_start_matches("import")
        .trim_start()
        .starts_with("static");
    let wildcard = declaration.contains(".*");
    let span = ByteSpan {
        start: node.start_byte(),
        end: node.end_byte(),
    };
    let mut fact = if is_static && !wildcard {
        // `import static a.b.C.member;` imports `member` of class `a.b.C`.
        let (class, member) = full
            .rsplit_once('.')
            .map(|(c, m)| (c.to_owned(), m.to_owned()))
            .unwrap_or((full.clone(), String::new()));
        let mut fact = builder.fact(span, "", class, ImportForm::Import);
        fact.imported_name = Some(member);
        fact
    } else {
        builder.fact(span, "", full, ImportForm::Import)
    };
    fact.glob = wildcard;
    if wildcard {
        fact.imported_name = Some("*".into());
    }
    if is_static {
        fact.scope = vec!["static".into()];
    }
    out.facts.push(fact);
}
