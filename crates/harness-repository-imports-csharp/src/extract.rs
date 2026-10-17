//! C# import extraction: `using` directives (plain, static, alias, global) and the namespaces and
//! top-level types a file declares, so a `using` can be matched to the files that declare its namespace.
use harness_workspace::{
    import_graph::{
        ByteSpan, DeclaredItem, ExtractedFile, FactBuilder, ImportForm, ImportLimits, ItemKind,
        LanguageImportAdapter, LanguageProfile, Visibility,
    },
    repository_intelligence::RootRelativePath,
};
use tree_sitter::Node;

pub const ADAPTER_VERSION: u32 = 1;

/// The item kind that stands for a `namespace` declaration (its name is the full dotted namespace).
pub const NAMESPACE_ITEM: ItemKind = ItemKind::Module;

#[derive(Clone, Copy, Debug, Default)]
pub struct CSharpImportAdapter;

impl LanguageImportAdapter for CSharpImportAdapter {
    fn id(&self) -> &'static str {
        "csharp-imports"
    }

    fn version(&self) -> u32 {
        ADAPTER_VERSION
    }

    fn profile_for(&self, path: &RootRelativePath) -> Option<LanguageProfile> {
        (path.as_path().extension()?.to_str()? == "cs").then_some(LanguageProfile::CSharp)
    }

    fn forms(&self) -> Vec<ImportForm> {
        vec![ImportForm::Use]
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
            .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
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
        walk_container(root, "", source, &builder, limits, &mut out);
        out
    }
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

fn span(node: Node<'_>) -> ByteSpan {
    ByteSpan {
        start: node.start_byte(),
        end: node.end_byte(),
    }
}

/// Visits the children of a compilation unit or namespace body. `namespace` is the dotted name so far.
fn walk_container(
    container: Node<'_>,
    namespace: &str,
    source: &str,
    builder: &FactBuilder<'_>,
    limits: &ImportLimits,
    out: &mut ExtractedFile,
) {
    let mut current = namespace.to_owned();
    let mut cursor = container.walk();
    for child in container.children(&mut cursor) {
        match child.kind() {
            "using_directive" => {
                if out.facts.len() >= limits.max_facts_per_file {
                    out.truncated = true;
                    continue;
                }
                using(child, source, builder, out);
            }
            "file_scoped_namespace_declaration" => {
                if let Some(name) = child.child_by_field_name("name") {
                    current = qualify(namespace, text(name, source));
                    out.items.push(namespace_item(name, &current));
                }
            }
            "namespace_declaration" => {
                let Some(name) = child.child_by_field_name("name") else {
                    continue;
                };
                let nested = qualify(namespace, text(name, source));
                out.items.push(namespace_item(name, &nested));
                if let Some(body) = child.child_by_field_name("body") {
                    walk_container(body, &nested, source, builder, limits, out);
                }
            }
            "class_declaration"
            | "interface_declaration"
            | "struct_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "record_struct_declaration"
            | "delegate_declaration" => {
                let Some(name) = child.child_by_field_name("name") else {
                    continue;
                };
                let kind = match child.kind() {
                    "interface_declaration" => ItemKind::Interface,
                    "enum_declaration" => ItemKind::Enum,
                    "struct_declaration" | "record_declaration" | "record_struct_declaration" => {
                        ItemKind::Struct
                    }
                    _ => ItemKind::Class,
                };
                let public = {
                    let mut inner = child.walk();
                    let found = child
                        .children(&mut inner)
                        .any(|m| m.kind() == "modifier" && text(m, source) == "public");
                    found
                };
                // The item's scope carries the namespace it was declared in.
                out.items.push(DeclaredItem {
                    name: text(name, source).to_owned(),
                    kind,
                    visibility: if public {
                        Visibility::Public
                    } else {
                        Visibility::Crate
                    },
                    span: span(name),
                    scope: vec![current.clone()],
                });
            }
            _ => {}
        }
    }
}

fn qualify(outer: &str, inner: &str) -> String {
    if outer.is_empty() {
        inner.to_owned()
    } else {
        format!("{outer}.{inner}")
    }
}

fn namespace_item(node: Node<'_>, name: &str) -> DeclaredItem {
    DeclaredItem {
        name: name.to_owned(),
        kind: NAMESPACE_ITEM,
        visibility: Visibility::Public,
        span: span(node),
        scope: Vec::new(),
    }
}

fn using(node: Node<'_>, source: &str, builder: &FactBuilder<'_>, out: &mut ExtractedFile) {
    let declaration = text(node, source);
    let is_static = declaration
        .trim_start_matches("global")
        .trim_start()
        .strip_prefix("using")
        .is_some_and(|rest| rest.trim_start().starts_with("static "));
    let alias = node
        .child_by_field_name("name")
        .map(|n| text(n, source).to_owned());
    let mut cursor = node.walk();
    let target = node
        .named_children(&mut cursor)
        .filter(|c| {
            matches!(
                c.kind(),
                "identifier" | "qualified_name" | "alias_qualified_name"
            )
        })
        .last();
    let Some(target) =
        target.filter(|t| alias.is_none() || Some(text(*t, source)) != alias.as_deref())
    else {
        return;
    };
    let mut fact = builder.fact(
        span(node),
        "",
        text(target, source).to_owned(),
        ImportForm::Use,
    );
    fact.alias = alias;
    if is_static {
        fact.scope = vec!["static".into()];
    }
    out.facts.push(fact);
}
